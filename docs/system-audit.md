# System audit — what to delete, archive, and fix

Measured 31 July 2026. Every number here came from inspecting the system, not
from estimation. Ordered by value, not by size.

---

## Summary

| Area | Finding | Action | Recovers |
|---|---|---|---|
| Local disk | 38.8 GB of Rust debug artifacts, 29.3 GB free | delete | **38.8 GB** |
| GitHub Actions | 5 upstream pipelines run on every push; 2 are broken | disable/fix | CI minutes + noise |
| Railway | 1 orphaned managed bucket | delete | small $, real confusion |
| Rust crates | 3 crates nothing depends on | delete / archive | ~17k LOC |
| Git history | 280 MB of deleted media still in the pack | decide now | **280 MB**, one-time |

---

## 1. Local disk — 38.8 GB reclaimable, and it matters

The repository is **45.7 GB** against **29.3 GB free**. That is the most urgent
item here, because a full disk fails builds in confusing ways.

| Path | Size | Verdict |
|---|---|---|
| `target/debug` | 27.94 GB | **delete** — regenerable |
| `desktop/src-tauri/target/debug` | 10.86 GB | **delete** — regenerable |
| `desktop/src-tauri/target/release` | 4.94 GB | keep — holds the running app |
| `.git` | 0.44 GB | keep (see §5) |
| `node_modules` (root) | 0.39 GB | keep |
| `~/.cargo` | 1.54 GB | keep — shared toolchain |

```bash
rm -rf target/debug desktop/src-tauri/target/debug
```

Cost: the next `cargo test` or debug build recompiles from scratch, roughly
10–20 minutes for this 28-crate workspace. Release builds stay incremental.

This regrows. In a workspace this size, periodically clearing `target/debug`
is routine maintenance, not a one-off.

**Also stale:** a git worktree from an earlier upstream comparison is still
registered at
`…/scratchpad/pristine3` (detached at `4d47aa834`). Remove with
`git worktree remove` (or `prune`) so `git worktree list` stays truthful.

---

## 2. GitHub Actions — running someone else's release pipeline

This is the least visible problem and the one most likely to bite.

Five workflows trigger on **every push**, and they were written for Block's
infrastructure, not ours:

| Workflow | Trigger | Problem |
|---|---|---|
| `docker.yml` | push | publishes to Block's registry namespace |
| `helm-chart.yml` | push | Block's Helm chart repo |
| `push-gateway-helm-chart.yml` | push | chart for a service we don't deploy |
| `sprig.yml` | push | builds `sprig`, a Block-internal tool (53 LOC) |
| `release.yml` | push (tags) | **broken twice over** — see below |

`release.yml` cannot succeed as written:

1. It builds `--features mesh-llm`. That feature was removed; the build errors.
2. Its updater endpoint is
   `github.com/hireshelby/hireshelby/releases/download/buzz-desktop-latest/latest.json`
   — **not this repository**. This one is the dangerous one: it fails *after*
   shipping, on customers' machines, which then silently never update. It must
   be fixed before the first release, because installed copies keep the URL
   they shipped with.
3. It signs macOS via `block/apple-codesign-action`, wired to Block's OIDC
   trust relationship.

**Recommended:** delete `sprig.yml` and `push-gateway-helm-chart.yml`; gate
`docker.yml` and `helm-chart.yml` behind `workflow_dispatch` until they point
at our own registry; fix the three faults in `release.yml`. Keep `ci.yml`.

The `*-canary.yml` workflows are `workflow_dispatch`-only, so they cost nothing
idle — archive at leisure.

---

## 3. Railway — one orphan

| Resource | State | Action |
|---|---|---|
| Bucket `buzz-media` (managed) | **orphaned** | **delete** |
| `minio` service + volume | in use | keep |
| `accounts`, `relay`, `site` | in use | keep |
| `Postgres` (accounts) | in use, 153 MB | keep |
| `Postgres-d6OJ` (relay) | in use, 162 MB | keep |

The managed bucket was provisioned, failed the relay's storage conformance
probe under rust-s3 0.37, and was replaced by the `minio` service. It still
exists and still holds probe objects. Nothing references it — deleting it
removes a decoy that will otherwise cost someone an hour someday.

```bash
railway bucket delete --bucket buzz-media
```

**Volumes are fine.** Four 5 GB volumes, each ~150 MB used. Railway bills on
usage, so provisioned headroom is free. The two Postgres instances are both
required — they hold independent sqlx migration histories and cannot share a
database (that mistake cost us a broken deploy already).

---

## 4. Rust crates — 3 orphans of 28

Reachability was computed from what we actually ship: the four container
binaries (`buzz-relay`, `buzz-admin`, `buzz-pair-relay`, `hireshelby-accounts`)
plus the desktop app and its five bundled sidecars (`buzz-acp`, `buzz-agent`,
`buzz-dev-mcp`, `git-credential-nostr`, `buzz`).

| Crate | LOC | Depended on by | Verdict |
|---|---|---|---|
| `sprig` | 53 | nothing (only Block workflows) | **delete** with `sprig.yml` |
| `buzz-pairing-cli` | 623 | nothing at all | **delete** |
| `buzz-test-client` | 16,361 | `ci.yml` only | **keep** — it is CI's integration harness |
| `buzz-push-gateway` | 4,092 | nothing we deploy | **archive** — APNs push; needed only if the mobile app ships |

`buzz-persona`, `buzz-ws-client`, `git-sign-nostr` and `countdown-bot` looked
orphaned in a naive scan but are reachable through the sidecars and examples.
They stay.

**`mobile/`** (7.4 MB, Flutter) is a decision, not a cleanup: keep it if iOS
and Android are on the roadmap, since `buzz-push-gateway` and the mobile
workflows only make sense alongside it. Otherwise archive all three together.

---

## 5. Git history — 280 MB, decide now or never

The pack is **443 MB**. Of all blob bytes ever committed, **280.7 MB (23.6%)**
are deleted media — the `goose-avatars` HEVC sets, ~4 MB per file. They are
gone from the working tree and alive forever in history, so every clone pays
for them.

Removing them means rewriting history (`git filter-repo`), which changes every
commit hash. That breaks existing clones and forks.

**The tradeoff is entirely about timing.** Right now the repository is public
but solo — no other contributors, no forks, no CI pinned to a SHA. The cost of
rewriting is close to zero today and rises permanently the moment someone else
clones it. If it is going to happen, it should happen before the first release.

If that feels risky, the honest alternative is to accept it: 280 MB is a slow
first clone, not a correctness problem, and never rewriting is a legitimate
choice. What is not legitimate is deciding later by accident.

Keep the `upstream` remote (`github.com/block/buzz`) either way — it costs
nothing and preserves the ability to pull upstream security fixes, which
matters for an Apache-2.0 derivative.

---

## Suggested order

1. **Delete `target/debug`** — 38.8 GB, zero risk, unblocks everything else.
2. **Delete the orphaned Railway bucket** — 30 seconds, removes a decoy.
3. **Disable the four upstream push-triggered workflows** — stops burning CI
   minutes on someone else's pipelines.
4. **Fix `release.yml`** — must happen before any release; item 2 there is
   unfixable in already-shipped copies.
5. **Delete `sprig` and `buzz-pairing-cli`.**
6. **Decide on the history rewrite** — the only item with a closing window.
