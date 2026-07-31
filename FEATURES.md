# HireShelby — Complete Feature Inventory

The full feature surface of the product, by component. Status is one of:
**shipped** (works today), **gated** (built, behind a preview flag),
**partial** (some endpoints/pieces missing), **planned** (designed, not built).

---

## 1. Desktop app (Tauri + React — the primary client)

27 feature areas under `desktop/src/features/`:

| Feature | What it does | Status |
|---|---|---|
| **channels** | Public/private channels, create/join/leave, membership | shipped |
| **chat** | Real-time messaging UI over the relay WebSocket | shipped |
| **messages** | Threads, replies, edits, deletes, `hireshelby://message` deep links, rich text (TipTap), markdown, spoilers, link previews (GitHub/Linear/Google) | shipped |
| **communities** | Multi-workspace: add/switch communities, hosted-community creation & onboarding | shipped (hosted flow: partial — see §7) |
| **community-members** | Member roster, roles, invites | shipped |
| **agents** | AI agents as first-class members: create/configure agents, personas, model tuning, run sessions, live transcripts, tool-call feed | shipped |
| **agent-memory** | Persistent agent memory (core memory blocks injected into prompts) | shipped |
| **huddle** | Voice huddles: join/leave, audio devices, push-to-talk, TTS/STT pipelines, agents in huddles | shipped |
| **search** | Full-text search across messages/channels (Postgres FTS) | shipped |
| **forum** | Forum-style threaded channels for long-form discussion | gated |
| **workflows** | YAML automations with message/reaction/schedule/webhook triggers and approval gates | gated |
| **projects** | Git repository browser, branches, PRs, contributor matching | gated |
| **pulse** | Activity feed: notes, social posts, agent activity | gated |
| **notifications** | Mention/DM notifications, per-channel preferences | shipped |
| **presence** | Online/offline, typing indicators (Redis-backed) | shipped |
| **user-status** | Custom status messages | shipped |
| **profile** | Display name, avatar, NIP-05 handle | shipped |
| **custom-emoji** | Community custom emoji, emoji-only message rendering | shipped |
| **reminders** | Scheduled reminders (NIP-ER relay support) | shipped |
| **moderation** | Report/hide/block, moderation queue | shipped |
| **onboarding** | First-run flow, identity creation, community join | shipped |
| **settings** | Appearance/themes, shortcuts, notifications, agents, hosted communities, experimental toggles | shipped |
| **channel-templates** | Prebuilt channel sets for new communities | shipped |
| **identity-archive** | Export/import identity keys | shipped |
| **local-archive** | Local message/agent-metric archive | shipped |
| **home** | Inbox/home view aggregating activity | shipped |
| **sidebar** | Navigation, unread badges, community switcher | shipped |

Desktop platform capabilities (in `src-tauri`):

- Nostr identity: keypair generation, OS-keyring storage, file fallback — shipped
- Agent runtimes: spawn/manage local agent CLIs (goose default, claude-code, codex) via ACP; readiness probes; instance reaping — shipped
- The Nest: persistent agent workspace at `~/.buzz` (AGENTS.md, skills, repos) — shipped
- BYO LLM providers: Anthropic, OpenAI, OpenAI-compatible, OpenRouter, Databricks ×2 — shipped
- Deep links: `hireshelby://` connect/join/message/add-community — shipped
- Auto-updater (endpoint repointed; no release published yet) — partial
- Media: upload, transcode, GIFs, snapshots; egress guard — shipped
- Native notifications, tray (macOS), window-state restore, webview zoom — shipped

## 2. Relay (`buzz-relay` — the hosted product)

- NIP-01 WebSocket relay; Schnorr-verified signed events — shipped
- NIP-42 auth (required), NIP-98 HTTP auth, API tokens — shipped
- Multi-tenant communities: host-based tenant binding before auth; fail-closed unknown hosts — shipped
- Channels (NIP-29 groups), DMs (NIP-17), reactions (NIP-25), replaceable events (NIP-16/33) — shipped
- REST bridge: /events, /query, /count, /media, /git, /hooks, /info — shipped
- Postgres FTS search (NIP-50) — shipped
- Media storage (Blossom/S3, MinIO local) — shipped
- Git hosting backend + NIP-34 patches/repo events — shipped
- Workflow engine (`buzz-workflow`): YAML triggers/actions — shipped
- Audit log: tamper-evident hash chain (`buzz-audit`) — shipped
- Reminders (NIP-ER), push-notification matcher (NIP-PL) — shipped
- Presence/typing via Redis pub/sub — shipped
- Operator API: community provisioning/archive/unarchive/list, NIP-98 + allowlist + replay guard — shipped
- Inter-relay QUIC mesh (`buzz-relay-mesh`) for multi-pod scale-out — shipped
- Admin dashboard (`admin-web`, read-only) — shipped
- Prometheus metrics, health/readiness probes — shipped

## 3. Control plane (`hireshelby-accounts` — new, ours)

| Endpoint | Purpose | Status |
|---|---|---|
| `GET /health` | Liveness + operator pubkey + DB status | shipped |
| `GET /v1/auth/login` | Browser login (WorkOS redirect or dev form) | shipped |
| `POST /v1/auth/dev-login` | Dev-only sign-in (off by default, 404s) | shipped |
| `POST /v1/auth/login/exchange` | Code → session (single-use, hashed) | shipped |
| `GET /v1/auth/me` | Session → user | shipped |
| `POST /v1/auth/logout` | Revoke session | shipped |
| `POST /v1/communities` | Provision tenant on relay + seed trial plan | shipped |
| `GET /v1/communities/list` | Session-scoped community list | shipped |
| `POST /v1/communities/:id/seats/check` | The per-seat paywall | shipped |
| `POST /v1/communities/availability` | Slug availability | **built this session** |
| `POST /v1/communities/archive` | Archive (proxies relay operator API) | **built this session** |
| `POST /v1/communities/unarchive` | Unarchive | **built this session** |
| `POST /v1/communities/transfer` | Transfer ownership (rotates owner pubkey) | **built this session** |
| `POST /v1/nostr-identities/challenge` | Mint signing challenge | **built this session** |
| `POST /v1/nostr-identities/verify` | Verify Schnorr sig, bind pubkey | **built this session** |
| `GET /v1/nostr-identities/current` | Bound identity for the session | **built this session** |
| `POST /v1/nostr-identities/delete` | Unbind | **built this session** |
| `POST /v1/billing/webhook` | Stripe webhooks (idempotent) → plan updates | **built this session** |
| WorkOS token exchange | Production login completion | **built this session** |
| Plans & quotas | trial/team/business/enterprise; pooled agent-hours; fail-soft | shipped |

## 4. Agent surface

- `buzz-acp`: headless ACP harness bridging relay @mentions → agent CLIs; idle/turn timeouts, session rotation, steering — shipped
- `buzz-agent`: built-in agent with direct provider transport, OAuth PKCE token source, skills discovery — shipped
- `buzz-cli`: agent-first CLI (JSON in/out): agents, channels, DMs, messages, reactions, emoji, feed, issues, patches, PRs, repos, moderation, memory, notes, social, uploads, users, workflows, packs — shipped
- `buzz-dev-mcp`: shell + file-edit MCP tools for agents — shipped
- Persona packs (`buzz-persona`) — shipped
- **Cloud agents** (metered, our infra, per §hireshelby-cloud-agents.md) — planned (Phase 4a)

## 5. Web & mobile

- `web/`: invite landing page + read-only git browser (repos, blobs) — shipped
- Full browser workspace client — planned (Phase 4b, after cloud agents)
- `mobile/`: Flutter iOS/Android clients (channels, chat, deep links, pairing) — partial (upstream "being wired up"); rebranded ids `com.hireshelby.mobile` |
- `admin-web`: operator dashboard — shipped

## 6. Git & identity tooling

- `git-sign-nostr`, `git-credential-nostr`: Nostr-signed git — shipped
- Pairing: `buzz-pair-relay`, `buzz-pairing-cli` (device pairing, NIP-AB) — shipped

## 7. Commercial layer (what makes it a business)

- Per-seat pricing; pooled cloud-agent-hour allowance; trial (14-day, no card) — designed; seat enforcement shipped
- Stripe: checkout, portal, tax, webhooks — webhook endpoint **built this session**; checkout/portal wiring on hireshelby.com — planned
- Relay-side quota enforcement at tenant bind — planned (B3)
- Code signing (Apple + Authenticode), release publishing — planned (C2)
- hireshelby.com (marketing, signup, download) — planned (Phase D, seeded from licensed Polaris)
- Included AI ("AI in Business tier" via provider with resale rights; GLM-5 self-host at volume) — planned (Phase E)

---

*Generated from the codebase at the commit noted in git history; the
"built this session" rows land in the same change as this file.*
