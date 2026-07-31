# HireShelby SaaS Plan

Status: proposal. Written against commit `cb319cd8f`.

This plan is grounded in what the code actually does today, not in what the
README implies. Where a claim came from reading source, the file is cited.

---

## 1. What the audit actually found

### 1.1 Builderlab is an account layer, not a platform

`desktop/src-tauri/src/builderlab.rs` calls exactly three groups of endpoints on
`https://app.builderlab.xyz/api/goose`:

| Group | Endpoints | What it does |
|---|---|---|
| Auth | `/v1/auth/login`, `/v1/auth/login/exchange`, `/v1/auth/me` | Browser login, local callback server on a loopback port, session credential in `X-BB-Session-Credential` |
| Identity binding | `/v1/buzz/nostr-identities/{current,challenge,verify,delete}` | Binds a Nostr pubkey to an account via challenge/response |
| Communities | `/v1/buzz/communities`, `/list`, `/availability`, `/archive`, `/unarchive` | Tenant CRUD |

**The relay already owns provisioning.** `crates/buzz-relay/src/api/operator.rs`
exposes `POST /operator/communities` (plus archive/unarchive/list), authenticated
by **NIP-98** signed requests, gated by the `RELAY_OPERATOR_PUBKEYS` allowlist and
`RELAY_OPERATOR_API_ORIGIN`, with replay protection
(`crates/buzz-relay/src/handlers/community_provisioning.rs`).

So Builderlab holds an operator keypair and calls the relay. It is a thin
control plane over an API we already have. **Replacing it is a bounded service,
not a platform rebuild.**

### 1.2 The web client is not the product

| | Files | Lines | Features |
|---|---|---|---|
| `web/src` | 48 | 4,160 | `invite`, `repos` only. Routes: `/`, `/repos`, `/repos/$repoId`, `/invite/$code` |
| `desktop/src` | 1,170 | 239,198 | 28 areas: channels, chat, messages, agents, huddle, workflows, forum, search, moderation, settings, … |

`web/` is an invite landing page plus a read-only git browser. **It is not a
workspace client.** "Ship web SaaS" is a port of ~239k lines, not a phase-1 task.

The coupling is narrower than the line count suggests:

- 51 of 1,170 files import `@tauri-apps` (~4.4%)
- 71 `invoke()` call sites
- 274 Tauri commands exist; 95 `src-tauri` files touch process spawn / keyring / filesystem

Most of the desktop UI already talks to the relay over WebSocket + REST, which
works unchanged in a browser. The seam is real but contained.

What genuinely cannot move to a browser: spawning local agent CLIs, OS keyring,
local filesystem/git checkouts, sidecar binaries.

### 1.3 Compute is already bring-your-own

Three independent paths, all shipped:

1. **ACP harness** (`crates/buzz-acp`) spawns the *user's own* agent CLI —
   `goose` (default), `claude-code`, `codex` (`config.rs:191`). Runs on the
   user's own Claude/OpenAI subscription.
2. **Built-in agent** (`crates/buzz-agent`) calls a provider directly. The
   desktop offers Anthropic, OpenAI, OpenAI-compatible, OpenRouter, Databricks
   (`desktop/src/features/agents/ui/agentConfigOptions.tsx:125-131`), each with
   the user's own key.
3. **Shared compute** (`mesh-llm`) — a community member shares their machine.

`buzz-acp` is configured entirely by `--relay-url` + `BUZZ_PRIVATE_KEY` env
(`config.rs:241-243`), so **it is headless and hostable server-side.**

---

## 2. The pricing answer

**Users keep their own subscriptions. We charge for the workspace, not for tokens.**

This is not a compromise — it is the strongest position available, and the
product is already built for it:

- **Our COGS is hosting** (Postgres, Redis, object storage, compute), not
  inference. Gross margin is infrastructure margin, ~80-90%, not the 10-30%
  spread of an LLM reseller.
- **No token price risk.** We never eat a provider price change or a runaway
  agent loop.
- **Lower objection at the door.** Teams already paying for Claude Max or
  ChatGPT Enterprise are not asked to pay twice. "Bring the AI you already
  have" is the pitch.
- **No inference liability.** We are not in the middle of the customer's model
  usage, their data retention terms, or their provider's TOS.

Optional upsell later, not at launch: **managed inference** for teams that do not
want to manage keys, priced as a metered add-on with a markup. Add it once the
core subscription is selling; it is a margin decision, not a launch dependency.

**What we bill for:** seats, number of communities, storage + retention, hosted
agent runtime hours, and enterprise controls (SSO, audit export, SLA).

---

## 3. Phase plan

### Phase 0 — Close the brand and legal gaps (days)

- Replace placeholder icons with the real HireShelby assets across Tauri, Android
  mipmaps, iOS AppIcon, favicon, DMG background.
- `desktop/src-tauri/src/builderlab.rs:120` still embeds the **Buzz bee SVG** and
  Buzz yellow `#d7d72e` in its auth-complete page. This file is deleted in
  Phase 1; if Phase 1 slips, strip the artwork sooner.
- Repoint placeholder URLs (`hireshelby/hireshelby`) to the real repo.
- Disable the publish workflows targeting a GHCR namespace we do not own.

### Phase 1 — HireShelby Accounts (the critical path)

A new service that replaces Builderlab. It must expose the same three endpoint
groups so the desktop client changes only its base URL and DTOs.

**Ship as `crates/hireshelby-accounts`** — a Rust + Axum service in this
workspace.

Why in-repo Rust rather than a separate Node/Next service:

- It must sign **NIP-98** operator requests to the relay. `buzz-sdk` and
  `buzz-auth` already do this; reimplementing Schnorr/NIP-98 signing in another
  language is avoidable risk on the security-critical path.
- Reuses the existing sqlx/Postgres setup, Docker build, and Helm chart.
- One language, one deploy pipeline, one on-call surface.

**Components:**

1. **Auth** — delegate to **WorkOS AuthKit**. Do not build password storage.
   B2B-first, and SAML/SCIM come free as we move upmarket, which is the
   enterprise upsell. (Keycloak ships in `docker-compose.yml` for local OAuth
   testing and can back local dev, but is too heavy to operate in production for
   a team this size.)
2. **Nostr identity binding** — port the challenge/verify flow. This is the one
   piece with no off-the-shelf equivalent; it proves a user controls a pubkey.
   Reuse `buzz-auth` verification primitives.
3. **Community CRUD** — hold the operator keypair, call the relay's existing
   `POST /operator/communities`. Our service must be in `RELAY_OPERATOR_PUBKEYS`.
   Operator key lives in a secret manager, never in the repo or an env file
   committed anywhere.
4. **Desktop client swap** — `hostedCommunityApi.ts` and the 6 UI files under
   `desktop/src/features/communities/` point at the new base URL. The loopback
   callback login flow in `builderlab.rs` is reusable almost as-is; rename to
   `accounts.rs` and repoint.

**Exit criterion:** a new user signs up on hireshelby.com, binds an identity,
creates a community, and connects the desktop app — with zero calls to
`builderlab.xyz`. Verify with a network trace, not by reading code.

### Phase 2 — Billing and quotas

- **Stripe.** Billing, Checkout, and Customer Portal — so we do not build card
  forms, dunning, invoices, or tax. Stripe Tax for VAT/sales tax.
- Webhooks land in `hireshelby-accounts`, which writes a `community_plan` row:
  plan tier, seat limit, storage limit, retention days, agent-hour allowance.
- **Enforcement belongs in the relay**, because the relay is the only component
  every client must pass through. Add a plan lookup to the existing tenant bind
  (`resolve_host` already runs before AUTH/EVENT/REQ), and enforce on: member
  add, media upload, event write, retention sweep.
- Fail *soft* on quota-read errors (allow, log, alert). Never let a billing
  lookup outage take down a paying customer's workspace.

**Exit criterion:** a downgrade actually blocks the next seat add, and an
over-quota upload is rejected with a clear, actionable error.

### Phase 3 — Sell desktop first

Desktop is the shippable product **today**. It is 28 feature areas and 239k
lines of working UI.

- Distribute as signed direct download from hireshelby.com. Not the Mac App
  Store: Tauri + MAS sandboxing is painful, and direct download keeps 100% of
  revenue with no review gate.
- Requires our own Apple Developer account and code signing —
  `.github/workflows/release.yml` currently uses `block/apple-codesign-action`
  with Block's OIDC and must be replaced.
- Windows: Authenticode signing cert.

This is the revenue path. It does not depend on Phase 4.

### Phase 4 — Hosted agents, then the web client

These are one project, in this order, because the first unblocks the second.

**4a. Hosted agents.** Run `buzz-acp` per tenant in our infrastructure. It is
already headless. This is simultaneously:
- a billable premium feature (agent-hours),
- the thing that makes agents work when the user's laptop is closed,
- and the prerequisite for a browser client, since a browser cannot spawn a
  local agent process.

Sandboxing is the hard part and must not be hand-waved: tenant-isolated
containers, no shared filesystem, egress limits, per-tenant resource caps. The
agent runs arbitrary shell commands by design.

**4b. Web client.** Port the desktop React app. The seam is the 51 files
importing `@tauri-apps` and 71 `invoke()` sites — replace each with an HTTP/WS
call or a browser equivalent. Ship in slices, highest value first:
channels/chat/messages → search → agents (via 4a) → workflows/forum. Huddles
(WebRTC) and local git are the long tail.

Realistic assessment: this is a multi-month port, not a sprint. Sequence it
after revenue exists.

---

## 4. Stack decisions

| Concern | Choice | Why |
|---|---|---|
| Control plane | Rust + Axum, in-repo crate | Must sign NIP-98; `buzz-sdk`/`buzz-auth` already do. One stack, one pipeline. |
| Auth | WorkOS AuthKit | B2B-first; SAML/SCIM as an upmarket upsell; keeps us out of password storage. |
| Billing | Stripe Billing + Checkout + Portal | Subscriptions and metered usage; Stripe Tax handles VAT/sales tax. |
| Database | Postgres (existing) | Already the relay's store; control plane reuses the sqlx setup. |
| Object storage | S3-compatible (existing Blossom/MinIO path) | Already implemented in `buzz-media`. |
| Hosting (launch) | Railway | Managed Postgres/Redis/storage; fastest path to production. Helm chart exists for the K8s move when scale demands. |
| Inference | **Bring your own** | See §2. Managed inference is a later margin play, not a launch dependency. |
| Marketing site | hireshelby.com, static | Pricing, docs, download links, Stripe Checkout entry. |

---

## 5. Sequencing and risk

**Order:** Phase 0 → 1 → 2 → 3 (revenue) → 4a → 4b.

Phases 1 and 2 are the only true blockers to charging money. Phase 3 is
packaging. Phase 4 is expansion.

**Top risks:**

1. **Operator key compromise.** The accounts service holds a key that can create
   and archive any tenant. Secret manager, rotation plan, audit every call.
2. **Hosted-agent sandbox escape** (Phase 4a). Agents execute shell commands.
   This is the highest-severity risk in the plan; budget real time for isolation
   and do not ship 4a on a deadline.
3. **Web port underestimation.** 239k lines. Do not promise a browser client to
   a customer before 4b is materially underway.
4. **Upstream drift.** `block/buzz` is actively developed and is set as
   `upstream`. Merge periodically in small batches; a six-month gap will be
   unmergeable.
5. **Quota enforcement outage.** Fail soft, always.

---

## 6. Open questions

- Pricing: per-seat, per-community, or hybrid? Needs a decision before Phase 2
  schema work.
- Self-host tier: offer a free self-hosted edition (drives adoption, Apache-2.0
  already permits it) or hosted-only?
- Do we keep `mesh-llm` shared compute as a differentiator, or cut it to reduce
  surface area?
