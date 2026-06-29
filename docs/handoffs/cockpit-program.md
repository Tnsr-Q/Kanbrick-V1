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
| P9 — BYO-AI Providers (cloud) | [#80](https://github.com/Tnsr-Q/Kanbrick-V1/issues/80) | 1, 2.3 | **P9.1–9.5 merged · P9.6 egress gate built — phase complete** (#101–#106) |
| P10 — Messenger + Visualizer | [#81](https://github.com/Tnsr-Q/Kanbrick-V1/issues/81) | 2.1, 2.2 | **P10.1–P10.7 merged (#120–#126) — phase complete end to end** |
| P11 — Skill/Loop Ecosystem | [#82](https://github.com/Tnsr-Q/Kanbrick-V1/issues/82) | 2.3, 2.5 | walking-skeleton **complete** (#127–#131); P11.4 provider steps (#132, ADR-0019); P11.5 MCP tool steps merged (#134, ADR-0020); **P11.6 authoring/library/loop-builder UI in flight** |
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
(cockpit CI builds it; added to the cockpit.yml path filter).

P9.6 closes the phase with the **egress gate** (`kanbrick-egress`): the one place core data may
leave, per ADR-0017. A `GatedTransport<T: HttpTransport>` decorates the P9.2 transport seam and
enforces three additive-only (restrict-only) checks before any socket opens — restrict-only RBAC
over `FirmContext.roles` (ported from `probes/rbac-overlay`, ADR-0010), a per-tenant **default-deny
host allowlist** (ADR-0017), and a **default-deny DLP** `(data-class → provider)` policy (ADR-0010,
orthogonal to clearance) — auditing every allow and deny via an `EgressAuditSink`. A denied call
never reaches the inner transport (no socket). Pure + fully offline-tested (8 tests incl. "denied →
0 socket calls"); the inner transport is injected (the real `reqwest` client at deploy time, or the
in-test stub — matching #106's stub-based verification). The ADR-0017 NetworkPolicy backstop is
implemented in `deploy/k8s/networkpolicy.yaml` (core pods egress-denied to the internet). **Phase 9
is complete.**

**P10 in flight (2026-06-27):** the seven slices are filed as discrete issues (#113–#119) and linked
under epic #81. **P10.1** (messenger backend, #113) is implemented on `claude/phase-10-handoff-8mp33g`:
a typed `MessengerEvent { actor, text, scope }` with a serde **internally-tagged** `MessengerScope`
(`public` | `group{name}`, mirroring a TS discriminated union 1:1) in `kanbrick-core::abi`, plus
`POST /me/messenger/send` (emit) and `GET /me/messenger/log?kind&limit` (replay) in `kanbrick-api`.
It rides the **existing** `EventBus` (typed events over the bus + `history()` replay — no new fabric),
resolves `actor` host-side from the validated `FirmContext` (never the request body, ADR-0002/0016),
and audits every send via `AuditLog`. Pure backend + integration-tested (the `provider_keys` route/test
template). **P10.1 merged as [#120].** **P10.2** (durability, #114) follows: each send now persists an
append-only `(:MessengerMessage)` to SparrowDB (the durable, authoritative history) via the ADR-0001
`MERGE`+`MATCH … SET` dialect; the `EventBus` replay log is bounded by a ring buffer
(`EventBus::with_capacity`, default 1024 on the control-plane bus) so it cannot grow without limit; and
`GET /me/messenger/log` reads the **durable store** (not the bounded bus), so history survives both bus
eviction and a process restart. **P10.2 merged as [#121].** **P10.4** (visualizer backend, #116) follows:
`GET /me/components` plus a cockpit `list_components` Tauri IPC enumerate every registered component with
live health counters — joining the `MeshRuntime` registry (name/version), the `GuestMetric` counters (the
same source as `/metrics`), and each guest's `GuestPolicy` clearance floor into a flat `ComponentStatus`
mirrored 1:1 to a TS type for the P10.5 UI. It is L4-gated and audited, and the IPC resolves identity
host-side (ADR-0016: the Bearer is injected from the host-held session, never the webview). **P10.4 merged
as [#122].** **P10.5** (visualizer UI, #117) follows: a React/Vite panel renders a card per component
(name, version, clearance badge, four live gauges) from `list_components`, with the gauges updated live by
a `watch_components` Tauri **Channel** stream that polls the host-side `GET /me/components` off the UI
thread (cancellable via `stop_watching`, mirroring the BYO-AI streaming pattern). Per-component clearance
is shown and a "Manage" affordance is gated by the viewer's `ClearanceLevel` (from `me()`) vs each
component's floor — presentation-only; the real bar stays server-side. `tsc --strict` + `vite build` are
green. **P10.5 merged as [#123].** **P10.3** (messenger UI, #115) follows: a React/Vite panel with a live
chat (composer + public/group scope selector) that posts via the P10.1 routes and streams the log back over
a `watch_messages` Tauri **Channel**, a lightweight **collaborative whiteboard** that rides the same stream
(strokes are messages scoped to a `whiteboard` group — no extra backend), notification popups on incoming
messages, and a local task list. Identity stays host-authoritative (the new `authed_post` injects the Bearer
host-side; the `actor` is stamped server-side, never the webview). `tsc --strict` + `vite build` green.
**P10.3 merged as [#124].** **P10.6** (internal-RPC self-registration, #118) follows: the internal RPC
surface gains `POST /internal/components/register` so a newly-added Rust sidecar/plugin self-registers a
`{name, version, clearance}` descriptor into an in-process `ComponentRegistry` (concurrency-safe, last-write-
wins) that the P10.4 `GET /me/components` folds in alongside the WASM guests. Registration is gated **solely**
by the constant-time `x-kanbrick-internal-token` (fail-closed; no JWT path) — reusing the same gate as the
graph/event callbacks — and an `executor`-side `register_component(cp_url, token, &descriptor)` helper is the
one-shot a sidecar calls at boot. Backend-only (the `kind` discriminator + UI land in P10.7). **P10.6 merged
as [#125].** **P10.7** (in-process service introspection, #119) closes the phase: `GET /me/components` now
unions **all three component kinds** — WASM guests, self-registered sidecars (#118), and the in-process
firm-OS services (the graph store, event bus, asset store, identity, capability registry, provider-key
custody, plus the executor forwarder + internal-RPC surface when the control-plane/executor split is wired,
so the service set reflects the live `AppState` configuration). Every row carries a `kind` discriminator
(`guest` | `sidecar` | `service`, names deduped in authority order guest > sidecar > service), mirrored 1:1
to the cockpit TS `ComponentKind`; the visualizer groups the three kinds, badges each, and shows live gauges
only for guests. `tsc --strict` + `vite build` green. **P10.7 merged as [#126] — Phase 10 (Messenger +
Visualizer, Req 2.1/2.2) complete end to end.**

**P11 in flight (2026-06-28):** the Skill/Loop Ecosystem (Req 2.3/2.5). The two hardest pieces already
exist, built + tested — the **run engine** (`kanbrick-mesh::Scheduler`: `schedule_with_retry`/
`schedule_interval`/`on_event`/`status`/`wait`, per-guest concurrency + timeouts) and the **permission
spine** (`kanbrick-discovery::ScopeGrants` dual-gate + the per-scope `Skill` primitive) — so P11 is mostly
wiring + the skill/loop schema + UI. Operator decisions (this session): **walking-skeleton-first** (make one
loop of guest-steps runnable through the real grant gate + a thin run-and-watch UI, then deepen);
a **skill = a versioned, grant-gated wrapper over a WASM guest** (the loop *step* is the polymorphic unit —
guest now; provider/MCP later); first usable cut is **guest-step loops only** (MCP tools P11.5 + per-step
keys P11.4 deferred). **P11.1** (skill schema/registry, ADR-0012) lands first: a new **`kanbrick-loops`**
crate holds the `SKILL.md` manifest + parser (pure `kanbrick-core`, so it builds + tests standalone), and
`kanbrick-store` (`skill_registry`) persists versioned `(:Skill)`/`(:SkillVersion)` nodes (ADR-0001 dialect,
append-only `seq`, `MERGE`+`SET` upsert). The registry is the catalogue only — `ScopeGrants` stays the sole
gate, wired in P11.2. Sequence: **P11.1 → P11.2** (grant + skill-authorization HTTP surface) **→ P11.3** (loop
schema + run engine on the Scheduler, ADR-0013) **→ thin P11.7** run-and-watch UI (the "usable" milestone)
**→ P11.4** keys / **P11.5** MCP / **P11.6** richer skill-library UI. **P11.1 merged as [#127].** **P11.2**
(grant lifecycle HTTP surface) follows — and was split from the skill bridge for a tighter review: it adds
`kanbrick-discovery` to `kanbrick-api` and exposes the `ScopeGrants` dual-gate over HTTP (`POST
/me/scope-requests`, `…/{id}/approve|deny`, `GET /me/scope-requests/{id}`, `GET /me/scopes`, `POST
/me/scopes/{id}/revoke`). Identity stays host-authoritative (the actor is the `AuthedContext`, never the
body); `approve`/`deny` build the firm org-graph per-request via `DiscoveryGraph::from_store` (always fresh
for the eligible-grantor chain check; caching deferred); the grant domain types aren't serializable so the
responses use thin DTOs. **P11.2 merged as [#128].** **P11.2b** (skill-registry ⇄ grant bridge) follows: a
new `skills` route module in `kanbrick-api` (adding `kanbrick-loops` as a dep) connects the P11.1 catalogue
to the gate. `POST /me/skills` parses a `SKILL.md` with `parse_skill_md`, **host-stamps** the author/`source`
from the authenticated identity (never the body), and publishes a versioned `SkillVersionRecord` (L3-gated;
malformed → `400 invalid_skill_md`); `GET /me/skills` + `GET /me/skills/{name}` browse the catalogue/history.
`POST /me/scopes/{id}/skills` binds a published edition onto an approved scope via `ScopeGrants::define_skill`
(picking up the edition's `min_clearance` as the run-time floor) — gated on scope **ownership** (or L5), and
deliberately **not** re-checking the binder's own clearance (`define ≠ run`; the run gate is P11.3), so an L2
scope owner may bind an L4-requiring skill. `GET` lists the bound skills (owner or L4). The registry record is
serde-returned directly; the grant `Skill` crosses as a thin DTO. **P11.2b merged as [#129].** **P11.3** (loop
run engine, ADR-0013) follows — the **"it runs"** milestone: a `(:Loop)`/`(:LoopStep)` schema
(`kanbrick-store::loop_registry`, ADR-0001 dialect) for owned, ordered pipelines (each step names a skill +
its `scope_id`); a thin **compiler** in a new `kanbrick-api` `loops` module that maps the steps onto the
**existing** `kanbrick-mesh::Scheduler` (`schedule_with_retry`/`wait`) and **gates each step at run time** via
`ScopeGrants::authorize_skill` (the runtime gate deferred from P11.2b — host base from
`ClearanceScope::resolve`), with defense-in-depth enforcement of the backing **guest's** policy floor so the
loop path is never a weaker door than `POST /guests/{name}`. The `Scheduler` is now wired into `AppState`
(built, not previously held), and a background executor pipes each step's output into the next, recording live
per-step `TaskStatus` to an in-process run registry (durable run history is P11.5). Routes: `POST/GET /me/loops`,
`GET /me/loops/{id}`, `POST /me/loops/{id}/run`, `GET /me/loops/runs/{id}`. Identity stays host-authoritative
(owner/caller from the `AuthedContext`); create/run audited, each authorized step self-audited. **P11.3 merged
as [#130].** **P11.7 (thin)** (run-and-watch UI) follows — the **"usable"** milestone: a Cockpit React
`LoopRunner` panel picks a loop, runs it, and watches each step's status live. It reuses the P10.5 Channel
poller verbatim (`cockpit/src-tauri/src/loops.rs` — std thread + `block_on` + `channel.send` + `AtomicBool`
cancel), but the `watch_run` stream **self-stops** once the run leaves `running` instead of polling forever.
New Tauri commands (`list_loops`/`run_loop`/`watch_run`/`stop_run_watch`) are pure HTTP clients of the bundled
kanbrick-api sidecar through the `authed_get`/`authed_post` bridge (the host-held Bearer is injected host-side,
ADR-0016; the webview supplies only the loop/run id + input — the server is the gate). `tsc --strict` + `vite
build` green; cockpit-Rust `fmt --check` green; clippy/test/`tauri build` gated by cockpit CI. **P11.7 merged
as [#131] — the walking skeleton is complete end to end** (author skill → publish → bind → compose loop → run
through the grant gate → watch live). **P11.4** (per-step provider keys, ADR-0019, **[HITL] — operator chose
skill-bound + seam-only this session**) follows, deepening the skeleton: the `(:LoopStep)` schema gains an
opaque `provider`/`model` (kept out of `kanbrick-store`'s dep graph); the executor branches guest-step vs
**provider step** — same `authorize_skill` gate (1A), then the host resolves the caller's key from
`AppState.provider_keys` **by `caller.user_id`** (never from the step) and injects it into a `ChatProvider`
built by an injected `ProviderFactory` seam (echo default; real adapter + `kanbrick-egress` `GatedTransport`
at deploy — 2A, no live `reqwest` in core/CI per ADR-0017). `provider_ref` selects the model only; a step can
never carry a credential or an identity (ADR-0002). Token-ledger recording is deferred to **P12**. **P11.4 merged
as [#132].** **P11.5** (external MCP tool steps, ADR-0020) follows — the **third step kind**: a `(:LoopStep)` is
now polymorphic across guest XOR provider XOR **mcp-tool**. A non-empty opaque `tool`/`tool_args` (kept out of
`kanbrick-store`'s dep graph, exactly like P11.4's `provider`/`model`) makes a step an external MCP tool-call; the
executor's third branch goes through the **same** `authorize_skill` gate (1A), then mints a per-invocation
capability bound to the caller's `FirmContext` (`InvocationCaps::mint`), hands an **injected `McpBridge` seam**
(`kanbrick-api/src/tool_runtime.rs`, mirroring the P11.4 `ProviderFactory`) **only** the opaque cap + the tool +
the args the scope authorizes (static `tool_args` merged with the piped payload under `"input"`), and **revokes
the cap** the instant the call returns. Per the P8.3 probe the real bridge wraps `tachyon-mcp` as a **managed
sidecar** (P7.2 `SidecarSupervisor`) over the `x-kanbrick-internal-token` channel — **not** a `HostServices`
backend, **no second WASM runtime** (ADR-0014), core stays no-egress (ADR-0017). This slice ships the seam + a
no-network stub default (injected via `AppState::with_mcp_bridge`); a recording-bridge test proves the opaque cap
resolves **host-side** to the caller (identity never in the step body), the tool, and the args. The create route
gains an optional per-step `tool_ref {tool, args?}` and rejects a step setting more than one kind (400). **P11.5
merged as [#134].** **P11.6** (skill-authoring + library + loop-builder UI, **[AFK]**, pure frontend — no new ADR,
reuses ADR-0011 + ADR-0016) closes the deepening half: a Cockpit **Skill Studio** React surface
(`cockpit/src/SkillStudio.tsx`) makes the create-side usable in the app. New Tauri commands
(`cockpit/src-tauri/src/skills.rs`, mirroring `loops.rs`) — `publish_skill`/`list_skills`/`skill_history`/
`bind_skill`/`list_scopes`, plus `create_loop` added to `loops.rs` — are pure HTTP clients of the bundled
`kanbrick-api` sidecar through the `authed_get`/`authed_post` bridge (host-held Bearer injected host-side,
ADR-0016; the webview supplies only the SKILL.md text / names / ids). The panel **authors** a SKILL.md (a
frontmatter form + body `<textarea>` composed client-side — the inverse of `SkillManifest::to_skill_md` — then
`POST /me/skills`, surfacing the host-stamped `source`/`seq`), **browses** the scope-filtered library with a
clearance badge + guest and per-skill version history, **binds** a published edition onto one of the caller's
active scopes (`GET /me/scopes?project=…` → `POST /me/scopes/{id}/skills`), and **builds** a loop of ordered
steps — each a guest XOR provider (`provider_ref {provider, model}`) XOR mcp-tool (`tool_ref {tool}`) — via
`POST /me/loops`, which then appears in the P11.7 `LoopRunner` to run/watch. Gated locally with `tsc --noEmit`
+ `vite build` (green) and `cargo +stable fmt` on `cockpit/src-tauri`; cockpit-Rust clippy/test/`tauri build`
ride the cockpit CI. **P11.6 in flight** — the **deepening half of Phase 11 is complete** once it merges (the
remaining P11.8 skill-publish trust gate is [HITL], for the operator).

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
file/function granularity · 0019 loop provider steps (host-injected key, model-only `provider_ref`) ·
0020 loop MCP tool-call steps (managed-sidecar bridge, capability passthrough, injected seam).
Each ADR is authored alongside its implementing slice (the repo's convention — see ADR-0008 landing
with Track G).

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
