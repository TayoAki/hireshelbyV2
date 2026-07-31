# HireShelby Cloud Agents + Per-Seat Pricing

Status: proposal. Companion to `hireshelby-saas-plan.md`. Written against `cb319cd8f`.

---

## 1. The model: hybrid, not either/or

Both, and they serve different customers:

| | Local agents (today) | Cloud agents (to build) |
|---|---|---|
| Runs on | User's machine, spawned by desktop | Our infrastructure |
| Agent CLI | User's own `goose` / `claude-code` / `codex` | Same CLIs, in our image |
| LLM credentials | User's own key or subscription | **Still the user's own** |
| Works when laptop is closed | No | Yes |
| Works from a browser | No | Yes |
| Billing | Included in seat | Metered per agent-hour |

**Users keep their own LLM subscription in both cases.** We are selling *runtime*,
not inference. That distinction is the entire margin story — see §3.

Cloud agents are also the prerequisite for a browser client: a browser cannot
spawn a local process, so web + agents requires server-side agents first.

### Why this is buildable

`crates/buzz-acp` is already headless. It takes `--relay-url` and
`BUZZ_PRIVATE_KEY` from env (`config.rs:241-243`), connects over WebSocket, and
spawns the agent CLI as a subprocess. Nothing about it assumes a desktop.

The cost-control primitives already exist and become our billing guardrails:

| Knob | Default | Billing role |
|---|---|---|
| `BUZZ_ACP_IDLE_TIMEOUT` | 900s | Stop billing on a stalled agent |
| `BUZZ_ACP_MAX_TURN_DURATION` | 7200s | Hard cap on a runaway turn |
| `BUZZ_ACP_MAX_TURNS_PER_SESSION` | 0 (off) | Session rotation |
| `BUZZ_ACP_LAZY_POOL` | false | Connect before spawning → scale-to-zero |

What must be built: the orchestration layer that provisions a sandbox per agent,
injects credentials, mounts persistent state, meters wall-clock, and enforces
quota.

---

## 2. Comparables — what the market actually charges

| Product | Model | Price | Includes inference? |
|---|---|---|---|
| **Devin** (Cognition) | Agent Compute Units — 1 ACU ≈ 15 min of autonomous work | $2.25/ACU PAYG; $2.00/ACU on Team. Tiers: Free, Pro $20/mo, Teams $80/mo + $40/seat, Max $200/mo | **Yes** |
| **GitHub Copilot** | Seat + included credits + overage. Moved fully to usage-based billing 1 Jun 2026 | Business $19/seat/mo incl. $19 credits; Enterprise $39/seat/mo incl. $39. 1 credit = $0.01 | **Yes** |

Devin's $2.00–2.25 per ACU works out to roughly **$8–9 per hour of agent work**,
because the ACU bundles both the sandbox and the model tokens.

Copilot's 2026 shape — **per-seat base + included allowance + metered overage** —
is now the industry-standard packaging for exactly this. We should copy it.

---

## 3. The margin insight

Sandbox infrastructure, 1 vCPU / 2 GB, as of 2026:

| Provider | ~Cost per hour |
|---|---|
| E2B | $0.05 – $0.083 |
| Daytona | $0.087 – $0.109 |
| Modal | ~$0.14 |
| Fly Machines / Sprites | Higher end; Sprites launched Jan 2026 for agents |

**Our cost to run an agent for an hour is roughly $0.05–0.14, because the
customer's own API key pays for the tokens.**

Devin charges ~$8–9/hour for the bundle. We are selling only the runtime half.
That means we can price at a small fraction of Devin and still hold 80–90% gross
margin — and the pitch writes itself: *bring the AI subscription you already pay
for; we run it in the cloud for you.*

Worked example at $0.60/agent-hour overage:

```
Revenue     $0.600 / agent-hour
Infra       $0.090 / agent-hour   (Daytona/E2B mid estimate)
Gross       $0.510 / agent-hour   = 85% margin
```

---

## 4. Proposed pricing

Per-seat base with a **pooled** org-wide cloud-agent allowance, then metered
overage. Pooled, not per-seat-enforced, because agent usage is spiky and
concentrated in a few power users — per-seat caps would punish exactly the
customers getting the most value.

| Tier | Price | Cloud agent hours (pooled/mo) | Notes |
|---|---|---|---|
| **Self-host** | Free | 0 | Apache-2.0, run your own relay. Unlimited local agents. Drives adoption. |
| **Team** | $24/seat/mo | 15 × seats | Hosted relay, unlimited local agents |
| **Business** | $48/seat/mo | 40 × seats | SSO/SAML, audit export, priority support |
| **Enterprise** | Contract | Negotiated | SLA, dedicated infra, SCIM |
| **Overage** | $0.60/agent-hour | — | Hard cap + alert, opt-in to exceed |

Unit economics at Team, $24/seat:

```
Cloud agent hours   15 × $0.09  = $1.35
Relay hosting share (pg/redis/s3/compute) ≈ $3.00
COGS                              ≈ $4.35   → ~82% gross margin
```

**Local agents are always unlimited and free.** They cost us nothing, and they
make the free/self-host tier genuinely useful, which is the top of the funnel.

---

## 5. Architecture

```
hireshelby-accounts (control plane)
  └── agent-runtime service
        │  provision / stop / status
        ▼
   Sandbox provider (E2B | Fly Machines | Daytona)
        │
        └── per-agent microVM
              ├── image: buzz-acp + goose + claude-code + codex
              ├── volume: the "nest" (~/.buzz persistent workspace)
              ├── env: BUZZ_ACP_RELAY_URL, BUZZ_PRIVATE_KEY, provider creds
              └── egress: relay + provider APIs only
                        │
                        ▼ WebSocket
                   buzz-relay (tenant-scoped)
```

**Provider choice.** Abstract behind a trait (`AgentRuntime`: `start`, `stop`,
`status`, `attach_volume`) and implement two. HireShelby agents are *channel
members* with persistent state, not one-shot code execution — so favour a
provider with cheap persistent volumes and fast stop/start (Fly Machines fits
that shape; E2B is cheapest and purpose-built for agent sandboxes). **Spike both
before committing**; the numbers above are close enough that operational fit
should decide, not price.

**State.** `managed_agents/nest.rs` defines a persistent agent workspace at
`~/.buzz` holding AGENTS.md, research, plans, logs, and repos. In cloud this
becomes a per-agent volume. It is the thing that makes an agent feel continuous
rather than amnesiac, so it must survive stop/start.

**Metering.** Bill wall-clock seconds from container-running to container-stopped,
not turn count — turns vary wildly in length. Emit to Stripe as metered usage.
Scale to zero on idle so a parked agent costs nothing.

**Credentials — the biggest liability.** Prefer OAuth over raw keys wherever the
provider supports it: `crates/buzz-agent/src/auth.rs` already implements an
RFC 6749 + 7636 PKCE `TokenSource` with refresh, alongside `StaticTokenSource`.
Where a raw key is unavoidable, envelope-encrypt with a KMS, inject at container
start, never write to the image, never log. Holding customer API keys is a real
breach-severity exposure and should be treated as such.

---

## 6. Build plan

Slots in as **Phase 4a** of the main plan — after billing exists, before the web client.

**4a.1 — Agent image.** Dockerfile with `buzz-acp` plus the three agent CLIs
pinned. Verify a container connects to a local relay and answers an @mention.
*Exit: an agent in a container responds in a channel.*

**4a.2 — Runtime abstraction + one provider.** `AgentRuntime` trait, first impl,
volume attach, scale-to-zero on idle.
*Exit: agent survives stop/start with its nest intact.*

**4a.3 — Credential vault.** KMS envelope encryption, OAuth-first, runtime
injection.
*Exit: a key is never present in the image, logs, or DB in plaintext.*

**4a.4 — Metering + quota.** Wall-clock accounting → Stripe metered usage;
quota checks in the control plane; hard cap with alerting.
*Exit: exhausting a pool blocks new agent starts with a clear error.*

**4a.5 — Isolation hardening.** microVM boundary, egress allowlist (relay +
provider APIs only), per-tenant CPU/mem/disk caps, no shared filesystem.
*Exit: an adversarial agent cannot reach another tenant or the control plane.*

**4a.6 — Desktop + web UI.** "Run in cloud" toggle per agent, hours-used meter,
upgrade path.

---

## 7. Risks

1. **Sandbox escape.** Agents execute arbitrary shell by design. This is the
   highest-severity risk in the whole roadmap. Use microVM isolation
   (Firecracker-class), not shared-kernel containers. Do not ship 4a.5 on a
   deadline.
2. **Credential compromise.** We would hold customer LLM keys. OAuth-first
   materially reduces this; treat the remainder as breach-severity.
3. **Abuse / cryptomining.** Metered compute with a free tier invites it. Require
   a card before any cloud-agent hours, cap concurrency, alert on sustained
   100% CPU.
4. **Runaway cost from a stuck agent.** Mitigated by existing `IDLE_TIMEOUT` and
   `MAX_TURN_DURATION`, but verify they actually stop the *container*, not just
   the turn — otherwise we bill for an idle box.
5. **Margin erosion from long-lived idle agents.** Scale-to-zero is not optional;
   it is the difference between 85% and negative margin.
6. **Provider lock-in.** The `AgentRuntime` trait exists for this reason. Keep
   the second implementation alive even if unused.

---

## 8. Sources

- Devin / Cognition ACU pricing: [Lindy](https://www.lindy.ai/blog/devin-pricing), [UsagePricing](https://www.usagepricing.com/blueprint/cognition), [usecarly](https://www.usecarly.com/blog/devin-pricing/)
- GitHub Copilot usage-based billing: [GitHub Blog](https://github.blog/news-insights/company-news/github-copilot-is-moving-to-usage-based-billing/), [UsageBox](https://usagebox.com/articles/github-copilot-usage-based-billing-2026)
- Sandbox infrastructure pricing: [Northflank](https://northflank.com/blog/ai-sandbox-pricing), [AgenticWire](https://www.agenticwire.news/article/e2b-vs-modal-agent-sandbox-cost-comparison), [AgentMarketCap](https://agentmarketcap.ai/blog/2026/04/07/ai-agent-sandbox-infrastructure-e2b-modal-daytona-fly-machines-secure-code-execution)
