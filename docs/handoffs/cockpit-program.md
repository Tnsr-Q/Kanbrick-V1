# Handoff — L5 "Cockpit" program (Phases 7–15)

> The agentic-desktop program that sits **on top of** the finished L1–L4 Firm OS.
> Read this with `docs/ARCHITECTURE.md`, the ADRs in `docs/adr/`, and the
> post-PRD handoff. Program tracker: **[#77](https://github.com/Tnsr-Q/Kanbrick-V1/issues/77)**.

## 1. What this is

The **Cockpit** is a **Tauri v2 desktop** that lets Kanbrick employees plug in their own
AI, run skills/loops, message/brainstorm, track tokens, see live file-access, and manage
the portfolio companies — first as the internal testbed, later mirrored per company.

The load-bearing decision: the Cockpit is **not a new spine**. It bundles the existing
`kanbrick-api` binary as a **Tauri sidecar** (`bundle.externalBin`) and composes the
already-complete `HTTP → Auth → Mesh → Guest → Graph` path. It re-implements nothing in
that spine. Every Tauri command rehydrates `FirmContext` from the session JWT in secure
storage — identity stays **host-authoritative across the IPC boundary**, exactly as
ADR-0002 requires across network hops (→ ADR-0016).

New host crates (in the workspace): `kanbrick-providers` (BYO-AI), `kanbrick-tokens`
(priced ledger + budgets), `kanbrick-loops` (skill/loop registry + run engine). New routes
are added to `kanbrick-api` reusing `require_clearance` + `AppState`. Frontend: a React +
Vite **webview**, kept swappable above the IPC boundary.

## 2. Synergize Tachyon-Mesh + Ironclaw without a clash (req 5)

Kanbrick already absorbed the **primitives** of both upstreams, not their runtimes:
`kanbrick-mesh` is built on wasmtime-45/WASIp1 directly (NOT Tachyon-Mesh `core-host`,
ADR-0002); `kanbrick-auth` is built on jsonwebtoken+argon2 directly (NOT the Ironclaw
binary). **There is no runtime fork.** The Cockpit takes only *bounded primitives*, each
behind a Phase-8 probe:

- **Tachyon-Mesh** → IOTA **Stronghold** (at-rest enclave for BYO provider keys — fills the
  `ApiKeyService` hash-only gap) + its **MCP server** (host bridge for loop tool-calls).
  **No second WASM runtime** (ADR-0014).
- **Ironclaw** → **RBAC + DLP as additive-only overlays**: roles may only *restrict*
  clearance (never elevate), reading the existing `FirmContext.roles`; DLP gates which
  provider a tenant's data may be sent to (ADR-0010).

`graphify-rs` is **not** an upstream to probe — it is already absorbed into
`kanbrick-discovery` (`graph.rs`/`codegraph.rs`/`influence.rs`). Req 6 *uses* it.

## 3. Operator decisions (2026-06-25)

| Decision | Choice | ADR |
|---|---|---|
| Frontend | **React + Vite webview**, kept upgradeable (swappable above IPC) | 0011 |
| Tenancy | **per-workstation CP + central approval queue** (Redis/Kafka, picked in P8.7) | 0015 |
| BYO-AI egress | **per-tenant provider-host allowlist + Ironclaw DLP**; core stays no-egress | 0017 |
| Local models | **deferred** — P9 is cloud-only (Claude/OpenAI/Cerebras); Gemma serving in P15 | — |

## 4. Reuse anchors (verified in source — build on these, don't rebuild)

| Need | Reuse |
|---|---|
| Messenger / whiteboard (req 2.2) | `EventBus` — `emit`/`subscribe`/`subscribe_typed`/`history`/`replay` (`kanbrick-mesh/src/event.rs`) |
| Loop run engine (req 2.3/2.5) | `Scheduler` — `schedule_with_retry`/`schedule_interval`/`on_event`/`RetryPolicy`/`TriggerHandle`/`TaskStatus` (`kanbrick-mesh/src/scheduler.rs`) |
| Token approval + per-project skills + permission design (req 2.4/6) | `ScopeGrants` dual-gate — request→`eligible_grantor`→approve/deny→`authorize_skill`→revoke/`expire_due`, fully audited (`kanbrick-discovery/src/grants.rs`) |
| Live file-access viz (req 6) | `GuardedStore` + `AuditLog` + `codegraph` (`kanbrick-auth/src/guarded.rs`, `audit.rs`, `kanbrick-discovery/src/codegraph.rs`) |
| Sidecar/plugin registration (req 2.1) | `caps.rs` `InvocationCaps`, `internal.rs` internal router + `x-kanbrick-internal-token`, `executor.rs` (`kanbrick-api/src/`) |
| BYO key custody (req 1) | Stronghold (P8.2) + `ApiKeyService` rotation pattern (`kanbrick-auth/src/apikey.rs`) |
| Workspace gating | `FirmContext`/`ClearanceLevel`, `require_clearance`, `ProjectScope` |

## 5. Phases (each an epic; each slice an independently-mergeable vertical slice)

| Phase | Epic | Requirement | Status |
|---|---|---|---|
| P7 — Cockpit Shell | [#78](https://github.com/Tnsr-Q/Kanbrick-V1/issues/78) | 2 | **built + CI-gated** (#87–#92) |
| P8 — Upstream De-Risk | [#79](https://github.com/Tnsr-Q/Kanbrick-V1/issues/79) | 3,4,5 | **ADRs landed + spikes green** (#93–#99) |
| P9 — BYO-AI Providers (cloud) | [#80](https://github.com/Tnsr-Q/Kanbrick-V1/issues/80) | 1, 2.3 | **P9.1–9.3 + 9.5 merged · P9.4 streaming UI built** (#101–#106) |
| P10 — Messenger + Visualizer | [#81](https://github.com/Tnsr-Q/Kanbrick-V1/issues/81) | 2.1, 2.2 | slices enumerated in epic |
| P11 — Skill/Loop Ecosystem | [#82](https://github.com/Tnsr-Q/Kanbrick-V1/issues/82) | 2.3, 2.5 | slices enumerated in epic |
| P12 — Token Tracking + Approval | [#83](https://github.com/Tnsr-Q/Kanbrick-V1/issues/83) | 2.4 | slices enumerated in epic |
| P13 — Graphify Access Visualizer | [#84](https://github.com/Tnsr-Q/Kanbrick-V1/issues/84) | 6 | slices enumerated in epic |
| P14 — Multi-Tenant | [#85](https://github.com/Tnsr-Q/Kanbrick-V1/issues/85) | 7 | slices enumerated in epic |
| P15 — Local model serving (deferred) | [#86](https://github.com/Tnsr-Q/Kanbrick-V1/issues/86) | 1 | tracking epic |

**P7 (Shell):** #87 scaffold · #88 sidecar bundle · #89 login+JWT custody · #90 IPC auth
contract (ADR-0016) · #91 `/me` panel · #92 CI e2e.

**P8 (De-Risk):** #93 init submodules + ADR-0014 · #94 Stronghold + ADR-0009 · #95 MCP
bridge · #96 Ironclaw RBAC/DLP + ADR-0010 · #97 frontend ADR-0011 · #98 egress ADR-0017 ·
#99 tenancy ADR-0015.

**P8 landed (2026-06-26):** all six ADRs (`docs/adr/` 0009, 0010, 0011, 0014, 0015, 0017)
+ three probe notes (`docs/probes/` p8.1 compat matrix, p8.2 Stronghold, p8.3 MCP bridge).
Two throwaway spikes are built + tested (std-only, excluded from the workspace):
`probes/rbac-overlay` (restrict-only RBAC + default-deny DLP, 6 tests) and
`probes/approval-queue` (single-writer serialization, no lost update, 3 tests); plus the
`cockpit/src/Spikes.tsx` UI surface spike (SVG graph + Canvas whiteboard) for ADR-0011.
Honest env note: the upstream submodule clone + the Stronghold round-trip are network-gated
(agent-proxy 403 / blocked tarball downloads) and reproduce on a network-capable machine / CI
— the Stronghold *dependency-closure* evidence (179 crates, no `core-host`) was captured here.

**P9 in flight (2026-06-26):** P9.1 (`kanbrick-providers` — `ChatProvider` trait + disjoint
`Usage`) merged via #107. P9.2 adds the **wire adapters** as pure codecs over an injected
`HttpTransport` seam (`wire.rs`): `anthropic.rs` (Claude Messages API — disjoint usage, struct-literal
map) and `openai.rs` (OpenAI **and** Cerebras Chat-Completions — *inclusive* usage via
`Usage::from_inclusive`, the double-count guard). No live `reqwest` ships here: ADR-0017 forbids core
egress until the **P9.6** allowlist+DLP gate exists, so the adapters carry no HTTP/TLS/async stack and
are fixture-tested with zero network (`RecordedTransport`). The real TLS transport wraps
`kanbrick-api::http_client` in P9.6.

P9.3 adds **per-employee key custody**: a `ProviderKeyStore` trait + `InMemoryKeyStore`
(`kanbrick-providers::custody`, namespaced by `FirmContext.user_id`) behind new
`POST/GET/DELETE /me/provider-keys` routes (`kanbrick-api`, `require_clearance` + `AuditLog`,
metadata-only reads). Cross-user reads are impossible by construction (outer map keyed on
`user_id`; routes read the host-authoritative JWT identity, never the path). Per ADR-0009 the
durable backends — IOTA Stronghold (primary) + OS keychain (fallback) — carry a native
`libsodium` dep and live on the **cockpit side**, injected via `AppState::with_provider_keys`;
the live enclave round-trip is deferred to a network-capable machine / cockpit CI
(`docs/probes/p9.3-key-custody.md`).

P9.5 adds the **priced token ledger** (`kanbrick-tokens`): each disjoint `Usage` bucket is
priced independently in integer **nano-USD** (`ModelPrice`/`PriceTable`), recorded per call to a
per-user `TokenLedger` (in-memory backend; aggregation reuses `Usage::accumulate`), with a
`Budget` value type. This is *capture + pricing* only — budget **enforcement** (central approval
queue, sweep) is P12.3 per ADR-0015. Fully workspace-side and CI-tested; no routes yet (the
usage UI is P12).

P9.4 wires BYO-AI streaming into the **cockpit** (Tauri v2): a `stream_completion` host command
opens a Tauri **Channel**, resolves the selected provider's key from host-side custody (the P9.3
`ProviderKeyStore`, in-memory now; Stronghold per ADR-0009), and streams `ChatProvider::stream`
deltas token-by-token to the webview — which sends only `{ provider, model, prompt }`, **never a
key** (ADR-0016). A React selector/console drives it, with cancel via a per-stream flag. P9.4
verifies headless with a no-network `EchoStreamProvider` stub; the real P9.2 adapters plug into the
identical `ChatProvider` interface at P9.6. The cockpit now depends on `kanbrick-providers` by path
(cockpit CI builds it; added to the cockpit.yml path filter). Remaining P9: **P9.6** DLP + egress
gate (#106) — the capstone that makes BYO-AI actually call out.

P7 and P8 run in parallel. Feature phases P9–P14 are **fully enumerated** in each epic body
(#80–#85) and are **filed as discrete issues phase-by-phase as each de-risk lands** (operator
decision, 2026-06-25) — so each slice's final shape reflects its probe's outcome.

### 5a. Staging — what unblocks filing which slices

The trigger is **specific**, not "all of P8": most *backend* slices gate only on the **P7 shell**
+ primitives that already exist; the *UI* slices gate on the **frontend ADR**; a few
security/topology slices gate on their one named probe. Closing a de-risk issue is the signal to
open its downstream slices.

| De-risk lands | ADR | Then file (as discrete issues) |
|---|---|---|
| **#93** P8.1 submodules / single runtime | 0014 | P9.1–9.2 provider crate + wire adapters; clears the ground for every other probe |
| **#94** P8.2 Stronghold enclave | 0009 | P9.3 per-employee key custody; informs P11.4 per-step key injection |
| **#95** P8.3 MCP bridge | — | P11 loop **tool-calls** (run engine works without it; this adds external tools) |
| **#96** P8.4 Ironclaw RBAC/DLP | 0010 | P9.6 DLP send-gate; restrict-only overlay used program-wide |
| **#97** P8.5 frontend (React+Vite) | 0011 | **every UI slice** — P10.3/10.5, P11.6/11.7, P12.5, P13.6, P14.6 |
| **#98** P8.6 egress allowlist | 0017 | P9.6 real outbound provider calls (pairs with #96) |
| **#99** P8.7 tenancy topology | 0015 | P14.2–14.6 CompanyState/routing/catalogs; P12.3 budget central-queue |

**Fileable the moment P7 lands (no P8 gate):** P10.1/10.2/10.4/10.6/10.7 (messenger + visualizer
backend), P11.1/11.2/11.3 (skill/loop schema + ScopeGrants routing + loop compiler), P13.1–13.5
(graph.access stream + ProjectScope granularity — ADR-0018 is authored inside P13.3 itself, not a
P8 probe), P14.1 (multi-org `FirmContext`). Don't hold these behind P8 — only their UI siblings
wait on #97.

The same map lives as a reverse index (close-probe → open-these) in a comment on the de-risk
epic **#79**, so closing a probe surfaces exactly which slices to open next.

## 6. ADR index (one-way doors)

`docs/adr/`: 0009 provider-key custody (Stronghold) · 0010 Ironclaw RBAC+DLP additive-only ·
0011 frontend (React+Vite, swappable) · 0012 unified skill model (SKILL.md ⇄
`(:Skill)`+`(:SkillVersion)`) · 0013 `(:Loop)`/`(:LoopStep)` run-engine on Scheduler+EventBus ·
0014 single WASM runtime · 0015 tenancy topology (per-workstation CP + central queue) ·
0016 Cockpit IPC auth contract · 0017 BYO-AI egress allowlist+DLP · 0018 ProjectScope
file/function granularity. Each ADR is authored alongside its implementing slice (the
repo's convention — see ADR-0008 landing with Track G).

## 7. Labels

Apply in the labeling pass (label *creation* needs repo access the issue tools lack, so
new labels are listed in each issue body as "Intended labels"):

- **new phase**: `phase-7`…`phase-15`
- **new layer**: `layer-5-cockpit`
- **new component**: `cockpit`, `providers`, `tokens`, `loops`, `messenger`, `multi-tenant`,
  `frontend`, `visualizer`
- **reused**: `needs-triage`, `enhancement`, `AFK`/`HITL`, `security`, `wasm`, `sparrowdb`,
  `tachyon-mesh`, `ironclaw`, `graphify-rs`, `layer-4-map`

## 8. Open questions still owned by the operator (deferred to P8 probes)

Stronghold-vs-OS-keychain fallback (P8.2); Ironclaw lib-vs-binary (P8.4); budget enforcement
sync-vs-sweep (default: sweep, like `expire_due`); skill-publish trust gate (default: dual-gate
lead review); per-employee vs per-company workspace state (default: per-company); central
approval-queue tech Redis-vs-Kafka and per-company DB placement (P8.7).
