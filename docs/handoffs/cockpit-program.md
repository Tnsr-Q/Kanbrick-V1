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
| P7 — Cockpit Shell | [#78](https://github.com/Tnsr-Q/Kanbrick-V1/issues/78) | 2 | slices filed (#87–#92) |
| P8 — Upstream De-Risk | [#79](https://github.com/Tnsr-Q/Kanbrick-V1/issues/79) | 3,4,5 | slices filed (#93–#99) |
| P9 — BYO-AI Providers (cloud) | [#80](https://github.com/Tnsr-Q/Kanbrick-V1/issues/80) | 1, 2.3 | slices enumerated in epic |
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

P7 and P8 run in parallel. Feature phases P9–P14 build on the P8 ADRs; their slices are
fully enumerated in each epic body and become discrete issues as the de-risk lands.

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
