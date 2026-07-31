# HireShelby Master Plan — Finish and Ship

Status: proposal. Written against `e1c0cbfc5`. Supersedes the sequencing in
`hireshelby-saas-plan.md`; that document's audit findings still stand.

---

## 0. Where we actually are

Shipped and verified against live services, not mocks:

| Capability | State |
|---|---|
| Rebrand + de-Block | Done. No Builderlab calls, no Block endpoints, no Buzz artwork. |
| Relay, 28 desktop feature areas | Inherited, working |
| Control plane: provisioning | Creates real tenants via NIP-98 on the relay |
| Control plane: login/sessions | Works end to end; loopback-only, hashed, single-use codes |
| Control plane: seat paywall | Trial denies, upgrade allows, downgrade re-denies |
| mesh-llm | Cut |

**The desktop app is not "unfinished" in its product surface.** It is 239k lines
across 28 feature areas. What is missing is the commercial wrapper around it.

The precise gap, measured by diffing what the desktop calls against what the
control plane serves:

```
MISSING (desktop calls, server does not serve):
  /v1/communities/archive
  /v1/communities/availability
  /v1/communities/transfer
  /v1/communities/unarchive
  /v1/nostr-identities/challenge
  /v1/nostr-identities/current
  /v1/nostr-identities/delete
  /v1/nostr-identities/verify
```

Eight endpoints. That is the functional remainder for the desktop client.

---

## 1. The AI monetization question — corrected

**A 90% markup on OpenRouter is not available to us.** Their terms, §7.4,
prohibit as a matter of conduct:

> "access the Site or Service for purposes of reselling API access to Models or
> otherwise developing a competing service"

And §12 prohibits sublicensing:

> "sell or otherwise transfer the access granted under these Terms"

Building the markup on OpenRouter would put the revenue line in breach from day
one, with the counterparty able to terminate the account that the feature
depends on. It is not a risk worth taking on a load-bearing revenue stream.

### The distinction that keeps the idea alive

There is a real difference between:

- **Reselling API access** — the customer is buying tokens from us. Prohibited
  by OpenRouter, and restricted by most aggregators.
- **Selling an application that uses inference internally** — the customer buys
  HireShelby; inference is an implementation detail we pay for. This is ordinary
  SaaS and is permitted by most provider terms.

Frame it as the second. "AI included in Business tier," not "tokens at a
multiple."

### Three viable routes

| Route | Terms risk | Margin | Notes |
|---|---|---|---|
| **Direct provider commercial agreement** (Anthropic / OpenAI / Google) | Low — read each ToS for an application-use clause | ~40–60% | Slowest to set up, cleanest story for enterprise |
| **Inference provider that permits resale** (Together, Fireworks, Baseten, DeepInfra, Z.ai) | Low — several offer explicit commercial/reseller terms | ~40–60% | Fastest path; negotiate resale rights in writing |
| **Self-hosted open weights (GLM-5, Apache-2.0)** | **None** — Apache-2.0 imposes no resale restriction | High at volume, negative below it | See §1.2 |

### 1.2 GLM-5 reconsidered

GLM-5 is Apache-2.0, 744B parameters / 40B active, and benchmarks at 81.0 on
Terminal-Bench 2.1 against Claude Opus 4.8's 85.0. Two of its advantages are
real and were understated earlier:

1. **No resale prohibition.** Apache-2.0 weights carry no ToS. This is the
   cleanest legal position available for bundling AI into a paid product.
2. **Data residency.** Self-hosting means customer prompts never leave our
   infrastructure — which answers the objection that Z.ai is a Chinese company,
   and is a genuine enterprise selling point.

The cost remains the blocker at our stage. At FP8 the weights are ~744GB,
needing roughly 8–16× H100 to hold, on the order of **$12k–35k/month for a
single node** before redundancy. That is a fixed cost carried whether or not
anyone uses it.

**Decision: buy wholesale now, revisit self-hosting at volume.** The trigger to
reconsider is a monthly inference bill that exceeds a reserved-GPU commitment —
at that point self-hosting flips from a liability to a margin and compliance win.

### 1.3 Pricing shape

Do not publish a per-token price. Model prices are public, so a visible multiple
is trivially discoverable and reads as gouging. Bundle instead:

| Tier | Price | AI |
|---|---|---|
| Team | $24/seat | Bring your own key. Unlimited local agents. |
| **Business** | **$48/seat** | **AI included** — generous monthly allowance, hard cap, overage opt-in |

Bring-your-own stays the default and the free path. Included AI is the
convenience upsell for teams that do not want to manage keys — which is exactly
the segment that will not price-shop tokens.

---

## 2. Polaris and NightCode — now licensed

Licenses were purchased, so both are usable. Keep the license terms on file and
confirm they cover commercial use in a closed-source product before shipping
code derived from them.

| Repo | Stack | Where it helps |
|---|---|---|
| **NightCode** | Bun, Hono, Prisma, **Clerk auth**, **Polar billing**, AI SDK streaming | A complete, small reference for the auth + billing + streaming wiring we are building. Most useful for the Stripe/webhook shape and usage metering. |
| **Polaris** | Next.js 16, React 19, Convex, CodeMirror, WebContainer | A working browser IDE. Relevant to the *marketing site* and, later, the web client. |

**Honest constraint:** our control plane is Rust/Axum and our client is
Tauri+React. Neither repo's code drops in — using them means porting patterns,
not copying files. Their value is as reference implementations that shorten
design time, not as a shortcut past the work.

Where each genuinely earns its keep:

- **NightCode → the billing model.** Read its Polar integration for the
  subscription/usage/webhook data model, then implement the same shape against
  Stripe in Rust. This is the highest-value borrowing available.
- **Polaris → hireshelby.com.** It is a polished Next.js app with auth and
  billing already wired. The marketing site plus signup and checkout is a real
  piece of work we have not started, and Polaris is a running head start on it.

---

## 3. The plan

Ordered so that revenue is unblocked as early as possible.

### Phase A — Close the control-plane gap (the 8 endpoints)

Everything the desktop already calls but nothing answers.

**A1. Nostr identity binding** (4 endpoints)
`challenge`, `verify`, `current`, `delete`. The schema exists
(`nostr_identities`, `identity_challenges`). Verification reuses `buzz-auth`
Schnorr primitives. Challenges are single-use and short-lived, same pattern as
login codes.
*Exit: a desktop user binds their key and it appears in `nostr_identities`.*

**A2. Community lifecycle** (4 endpoints)
`availability` (slug free?), `archive`, `unarchive`, `transfer`. Archive and
unarchive proxy to the relay operator API, which already implements both.
Transfer rotates the owner pubkey — the same operator call, and the most
dangerous of the four, so it needs an explicit ownership check.
*Exit: the desktop settings card manages communities with no 404s.*

**A3. Contract test.** Assert every `/v1/*` path in `accounts.rs` has a route in
the control plane. This gap was found by hand once; it should not be findable by
hand twice.
*Exit: a missing endpoint fails CI, not a user.*

### Phase B — Take money

**B1. WorkOS token exchange.** The redirect half is wired; the exchange call is
not. Until it lands, production login does not work and dev-login is the only
path — which must never ship enabled.
*Exit: a real user signs in on hireshelby.com with no dev bypass.*

**B2. Stripe.** Checkout, Customer Portal, Tax. Webhooks write `community_plans`.
The idempotency table (`processed_stripe_events`) already exists because
webhooks retry and arrive out of order.
*Exit: a card charge upgrades a trial to Team and the seat cap lifts, verified
against the live seat-check endpoint.*

**B3. Relay-side enforcement.** Seat checks currently answer a question; the
relay does not yet ask it. Wire the quota lookup into the relay's tenant bind,
which already runs before AUTH/EVENT/REQ. Fail soft on lookup error.
*Exit: an over-quota member add is refused by the relay itself.*

### Phase C — Ship the desktop app

**C1. Real brand assets.** Replace the placeholder monogram across Tauri icons,
Android mipmaps, iOS AppIcon, favicon, and DMG background. *Blocked on the logo
files.*

**C2. Code signing.** `.github/workflows/release.yml` still uses
`block/apple-codesign-action` with Block's OIDC. Needs an Apple Developer
account ($99/yr) and an Authenticode certificate for Windows.
*Exit: a signed .dmg and .exe install without an OS warning.*

**C3. Auto-update.** The updater endpoint is repointed but no release has been
published to it. Verify an installed build updates itself.

**C4. Feature-gate triage.** `preview-features.json` gates workflows, projects,
pulse, forum, and agent-managed profiles. Decide per feature: ship on, ship off,
or cut. Shipping a half-finished feature visible by default is worse than not
shipping it.

**C5. README screenshots.** Still images of Block's Buzz UI, bee icon included.
Retake once C1 lands.

### Phase D — hireshelby.com

Marketing, pricing, signup, Stripe Checkout, download links. **Start from
Polaris**, which already has auth and billing wired in Next.js.

### Phase E — Included AI (§1)

Negotiate resale terms with one inference provider. Add a managed provider
option in the desktop that requires no key. Meter and cap. Revisit self-hosted
GLM-5 when the inference bill exceeds a reserved-GPU commitment.

---

## 4. Sequencing

```
A1 ─ A2 ─ A3 ────────┐
                     ├─ C (ship desktop) ─ D (site) ─ E (AI)
B1 ─ B2 ─ B3 ────────┘
```

A and B are independent and can run in parallel. **Nothing ships without both.**
C is packaging. D and E are growth.

Critical path to first dollar: **B1 → B2**. Everything else can slip.

---

## 5. Risks

1. **Reselling terms.** Get resale rights in writing before building Phase E on
   any provider. §1 exists because the obvious choice was already foreclosed.
2. **Dev-login in production.** It issues a session for any email. It is off by
   default and 404s when disabled, but a deployment checklist must assert
   `dev_login_enabled: false` in the startup log.
3. **The operator key.** Still the highest-privilege secret; compromise is
   compromise of every tenant.
4. **Feature gates.** Five gated features are a decision debt, not a feature set.
5. **Upstream drift.** `block/buzz` is actively developed and set as `upstream`.
   Merge in small batches.
6. **Windows CI.** Two pre-existing test-suite failures are environmental
   (POSIX shell, path separators) and reproduce on upstream HEAD. They will fail
   CI on Windows runners until fixed or skipped by platform.
