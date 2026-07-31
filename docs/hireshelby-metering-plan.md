# Metered inference: turning HireShelby into a paid product

How users go from download → trial → paying, and how we bill agent work across
many models with one unit.

Status: plan. Nothing in this document is built yet except where noted.

---

## 1. What the comparables actually do

Two models dominate, and they make opposite trade-offs.

**Cursor — dollar-pegged credits.** Each plan includes a credit pool roughly
equal to the plan price (Pro $20 → ~$20 of model usage). Beyond the pool, calls
bill at the provider's own API price plus roughly 20%. Usage markup, not the
subscription, is the profit engine. Users can reason about cost because credits
are just dollars.

**Devin — abstract normalized units.** An ACU (~15 minutes of active work)
bundles VM time, inference, and network into one opaque unit at $2.00–2.25.
Core is $20/month platform + pay-per-ACU; Team is $500 for 250 ACUs. The
abstraction lets Cognition bundle non-inference cost, but consumption is
famously hard to predict — a task needing five iteration loops costs far more
than a one-pass task that looks identical.

**What this means for us.** We have both cost types: inference *and*
platform/compute (relay, Postgres, MinIO, and cloud agent VMs when those land).
So we need a unit that can absorb both — Devin's insight. But our buyers are
developers who will resent an opaque unit — Cursor's insight. Take both:
a normalized unit that is *published as dollar-pegged*.

---

## 2. The unit

**1 Shelby Credit (SC) = $0.01 of metered cost.** Credits are dollars with the
decimal point moved, so "you have 1,200 credits left" means "$12 of agent work
left" and nobody has to learn a new currency.

Every billable action reduces to the same formula:

```
raw_cost  = (in_tokens  / 1e6) * model.input_price
          + (out_tokens / 1e6) * model.output_price
          + (cache_read_tokens / 1e6) * model.cache_read_price
          + vm_seconds * vm_rate                    -- cloud agents, later
credits   = ceil(raw_cost * MARKUP / 0.01)
```

**This is the answer to "can we meter many models the same way."** Yes —
because every model collapses to `raw_cost` through a price table. Adding a
model is adding a row, not writing new billing code. GLM-5, Claude, GPT, a
local model at zero cost: same formula, same ledger, same invoice line.

`MARKUP` starts at **1.20**, matching Cursor's overage convention. It is a
config value, not a constant in code.

### The price table must be versioned

Provider prices move, and ours must move with them without a redeploy or a
rewrite of history.

```sql
CREATE TABLE model_prices (
    model_id           TEXT        NOT NULL,   -- 'zai/glm-5'
    provider           TEXT        NOT NULL,   -- 'zai'
    input_price        NUMERIC(12,6) NOT NULL, -- USD per 1M tokens
    output_price       NUMERIC(12,6) NOT NULL,
    cache_read_price   NUMERIC(12,6) NOT NULL DEFAULT 0,
    effective_from     TIMESTAMPTZ NOT NULL,
    effective_to       TIMESTAMPTZ,            -- NULL = current
    PRIMARY KEY (model_id, effective_from)
);
```

Usage is priced with the row in effect **at the time of the call**, so a price
change never retroactively rewrites what a customer already owes.

> This is not hypothetical. GLM-5 is currently $0.60/$1.92 per 1M *at 40% off*.
> When that promo ends, our cost rises ~66% overnight. With a versioned table
> that is one INSERT; with prices in code it is an incident.

---

## 3. Plans

Seat price covers the platform (relay, workspaces, git, storage,
collaboration) — that revenue is ~100% margin and does not depend on inference.
Credits cover agent work on top.

| Plan | Price/seat | Credits included | Our inference cost | Gross margin |
|---|---|---|---|---|
| Trial | free, 14 days | 500 (~$5) | ~$5 worst case | negative, capped |
| Team (BYO) | $24 | 0 — bring your own key | $0 | ~100% |
| Team + Managed | $39 | 1,500 (~$15) | ~$5 typical | ~87% |
| Business + Managed | $69 | 4,000 (~$40) | ~$15 typical | ~78% |

Overage is **opt-in and off by default**: at zero credits, agents stop and the
user is asked to top up or enable pay-as-you-go. Cursor bills overage at
pass-through with no penalty markup; we match that — the 20% is already in the
credit price.

**Keep BYO as the headline plan.** "Use the Claude Max subscription you already
pay for" is the differentiator against Cursor and Devin, and it carries zero
inference risk. Managed is the convenience upsell, not the default.

---

## 4. Legal boundary (checked, not assumed)

Z.ai ships two products with **opposite** terms, and picking the wrong one puts
the whole business on a violation:

- **GLM Coding Plan** (the $3–15/mo subscription) — cannot power HireShelby.
  It forbids using quota for "directly invoking model APIs from their own
  applications, bots, websites, SaaS products", and separately bars
  resell / repackage / **proxy**.
- **Pay-per-token API** — this is the one. Its license explicitly grants "the
  right to use Z.ai's API to integrate the Services into applications or to
  develop downstream systems, applications or functions for end users", and
  makes the developer responsible for establishing agreements with their end
  users (our ToS and Privacy Policy already do this).

The gateway must therefore authenticate to Z.ai with a **pay-per-token API
key**, never a Coding Plan subscription credential. Worth a comment at the
config site, because the two look identical in a `.env`.

The same split applies elsewhere and must be re-checked per provider before
enabling it in Managed mode. OpenRouter's terms prohibit "reselling API access
to Models" outright, so OpenRouter stays **BYO-only** — users may point their
own key at it, we may not resell it.

---

## 5. Architecture

A new `hireshelby-gateway` crate. It is the only component that holds provider
keys.

```
desktop / buzz-agent / harness
        │  ANTHROPIC_BASE_URL=https://gateway.hireshelby.com
        ▼
┌─────────────────────────────────────────┐
│ hireshelby-gateway                      │
│  1 authenticate (HireShelby session)    │
│  2 resolve plan + balance               │
│  3 reject if exhausted  ← fail closed   │
│  4 proxy to provider, stream through    │
│  5 read usage, write ledger entry       │
│  6 async → Stripe meter event           │
└─────────────────────────────────────────┘
        ▼
  Z.ai · Anthropic · OpenAI
```

**The client work is already done.** `buzz-agent` reads `ANTHROPIC_BASE_URL`
([config.rs:784](../crates/buzz-agent/src/config.rs)) and
`OPENAI_COMPAT_BASE_URL`, and spawned harnesses inherit user env last
([agent_model_process.rs:53](../desktop/src-tauri/src/commands/agent_model_process.rs)).
So "HireShelby Managed" is a provider preset pointing at the gateway. No agent
code changes.

### Five things that are easy to get wrong

**1. Metering must survive a disconnect.** If the user cancels mid-stream we
have already paid the provider for the tokens generated. Record usage on
completion *and* on abort, from whatever the provider reported. Anything else
is a free-tokens exploit: start a huge generation, disconnect, repeat.

**2. Cost is unknown until the call finishes.** Use reserve → settle: reserve
`max_tokens * output_price` at admission, settle the actual on completion,
release the difference. Without a reservation, N concurrent requests each see
the same balance and all pass the check.

**3. The hard cap needs headroom.** A request admitted with 1 credit left can
still spend the value of `max_tokens`. Real exposure is
`balance + (max_tokens * output_price * MARKUP)`. Bound `max_tokens` per plan
so worst-case overshoot is a known, small number.

**4. The local ledger is the enforcement authority, Stripe is for invoicing.**
Never call Stripe synchronously in the request path — it adds latency and its
outage becomes our outage. Write locally, push meter events asynchronously,
reconcile nightly.

**5. Idempotency.** Stripe enforces meter-event uniqueness over a rolling 24h
window, so use our `usage_id` (UUID) as the event identifier — retries after a
timeout then collapse instead of double-billing. This mirrors the
insert-as-lock idempotency already used for Stripe webhooks in
[billing.rs](../crates/hireshelby-accounts/src/billing.rs).

### Ledger

```sql
CREATE TABLE usage_events (
    id              UUID PRIMARY KEY,
    community_id    UUID        NOT NULL REFERENCES communities(id),
    account_id      UUID        NOT NULL,
    model_id        TEXT        NOT NULL,
    input_tokens    BIGINT      NOT NULL,
    output_tokens   BIGINT      NOT NULL,
    cache_read_tokens BIGINT    NOT NULL DEFAULT 0,
    raw_cost_usd    NUMERIC(12,6) NOT NULL,  -- what we paid
    credits         BIGINT      NOT NULL,    -- what they owe
    occurred_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    reported_at     TIMESTAMPTZ              -- NULL until Stripe accepts
);
CREATE INDEX ON usage_events (community_id, occurred_at DESC);
CREATE INDEX ON usage_events (reported_at) WHERE reported_at IS NULL;
```

Storing `raw_cost_usd` alongside `credits` means margin per customer is a
query, not a spreadsheet — and it is how we notice a plan losing money while
it is still cheap to fix.

---

## 6. The commercial loop

1. **Download** — signed installer from hireshelby.com. Windows signing is
   still outstanding (Azure Trusted Signing, ~$10/mo); unsigned downloads get
   a SmartScreen warning that will cost more conversions than the cert costs.
2. **Trial** — 14 days, 500 credits, no card. Already modelled as the `trial`
   plan in the control plane.
3. **Convert** — Stripe hosted Checkout, already built and live-tested
   (trial/1 seat → team/5 seats verified end to end).
4. **Use** — BYO key, or Managed via the gateway.
5. **Expand** — seat quota enforcement is built ([quota.rs](../crates/buzz-relay/src/quota.rs));
   credit quota reuses the same fail-soft client pattern.

---

## 7. Build order

**Phase 1 — meter without charging.** Gateway proxies to Z.ai, writes
`usage_events`, enforces nothing. Run it on our own team for two weeks. The
goal is a real distribution of credits-per-session before any price is
committed to — every guess in §3 is currently a guess.

**Phase 2 — enforce.** Reserve/settle, balance checks, hard caps, the "out of
credits" state in the desktop UI. Still no money.

**Phase 3 — charge.** Stripe meters, credit grants on subscription renewal,
top-ups, overage opt-in, usage dashboard.

**Phase 4 — widen.** Add Anthropic and OpenAI as Managed providers (each needs
its own terms review per §4), then cloud-agent VM seconds through the same
`raw_cost` formula.

Phase 1 is small and answers the pricing question with data. Phases 2 and 3 are
where the correctness traps in §5 live and deserve the most test coverage.

---

## Sources

- [Cursor pricing analysis](https://www.vantage.sh/blog/cursor-pricing-explained) · [business model breakdown](https://valueaddvc.com/blog/how-does-cursor-make-money-subscriptions-token-pricing-and-the-business-model-breakdown)
- [Devin ACU pricing](https://www.usecarly.com/blog/devin-pricing/)
- [Z.ai Terms of Use](https://docs.z.ai/legal-agreement/terms-of-use) · [Subscription Terms](https://docs.z.ai/legal-agreement/subscription-terms)
- [Stripe advanced usage-based billing](https://docs.stripe.com/billing/subscriptions/usage-based/advanced/about) · [Meter Events API](https://docs.stripe.com/api/billing/meter-event)
