# ADR 0004 — Phase 5 business guests: SDK shape, guest crate architecture, and the valuation model

- **Status:** Accepted
- **Date:** 2026-06-16
- **Context:** Phase 5 (Business Logic — WASM guests) — issues #39–#46. Builds on
  the Phase 3 host↔guest ABI (#22, ADR-0002) and the Phase 2/4 clearance model.
- **Deciders:** Phase 5 agent + operator (HITL: #45 financials & DCF parameters).

## Context

Phase 5 turns the three guest scaffolds into real `wasm32-wasip1` business
modules driven by a typed SDK. Before building we confirmed the upstream
realities the same way prior phases did:

- **`kanbrick-core` compiles to `wasm32-wasip1`** (chrono, uuid/getrandom, serde,
  thiserror all build), so the SDK can **reuse** the shared `abi` types with zero
  duplication (#39 acceptance) — host and guest cannot diverge on the wire shape.
- The Phase 3 runtime already publishes `kbk_ctx_*` (#23) and `kbk_query_graph`
  (#24) as WASM imports under the `"kanbrick"` module, proven clearance-filtered
  end-to-end. **`emit_event` and `log` existed only on the host `HostFunctions`
  trait, not as WASM imports** — Phase 5 wires them.

## Decisions

1. **Two new host imports.** `kanbrick-mesh` gains `kbk_emit_event(ptr, len)`
   (publishes an `Event` onto a bound `EventBus`, #27/#46) and
   `kbk_log(level, ptr, len)` (records at a `LogLevel`). `MeshRuntime::with_bus`
   binds the bus; with no bus an emitted event is logged and dropped. These join
   the context/query imports in the `run_with_context` path only (the raw
   `dispatch` path stays import-free).

2. **`kanbrick-guest-sdk` (#39) reuses the core ABI, with target-split bindings.**
   The SDK re-exports `kanbrick_core::abi` types and exposes `firm_context()`,
   `query_graph()`, `emit()`, `log()`, plus a `guest_entrypoint!` macro that emits
   the `kbk_alloc`/`kbk_run` exports (ADR-0002 calling convention). The `kbk_*`
   imports and memory glue are `#[cfg(target_arch = "wasm32")]`; on the host
   target the capability functions are `unimplemented!()`. **Rationale:** a guest's
   *pure logic* is unit-tested natively (PRD testing strategy) and never calls the
   SDK's IO; only the thin wasm entrypoint does. This keeps one workspace, lets
   the SDK be a normal member (host build + clippy clean), and reserves the real
   behaviour for wasm — verified by the `guests/sdk-example` reference guest run
   through the mesh.

3. **Guest crate architecture: logic as a native-testable `rlib`, entrypoint
   `#[cfg(wasm32)]`.** Each business guest (`guests/{valuation,reporting,
   compliance}`) stays a **workspace member** with `crate-type = ["cdylib",
   "rlib"]`. Pure business logic (DCF math, compliance rules, dashboard
   aggregation) lives in functions that take already-fetched data and is unit
   tested natively; the wasm entrypoint (SDK glue: read context, `query_graph`,
   call the logic, respond/emit) is `#[cfg(wasm32)]`. They build to wasm via
   `cargo build --target wasm32-wasip1 -p <guest>` (#40) and are loaded by mesh
   integration tests (the "WASM matches native" requirement). `guests/echo` and
   `guests/sdk-example` are wasm-only fixtures, excluded from the workspace and
   built by the mesh build script.

4. **A guest error is a structured response, never a trap/panic.** The SDK
   entrypoint converts a handler `Err` into a `GuestResponse` payload
   `{ error, kind }` (PRD "malformed input → structured error, no panic").
   Clearance rejection (e.g. valuation requires L3+, compliance L4+) is enforced
   **in guest logic** by reading `firm_context().clearance` and returning such an
   error response — not by relying on a host trap.

5. **Valuation financials = hybrid, with provenance (#45, operator-approved).**
   The seed carries no financials, so a new migration adds
   `(:Company)-[:HAS_FINANCIALS]->(:FinancialSnapshot { revenue, ebitda, fcf,
   growth_rate, net_debt, quarter, source_tag })` for the 9 companies, **tagged
   `source_tag: "SYNTHETIC"`**. The valuation guest uses **request-payload
   financials when supplied (authority), else the graph snapshot (default)**, and
   reports which was used (`FinancialsSource::{UserProvided, GraphDefault}`) plus a
   warning when the data is synthetic. The run is audited. This avoids "fake data
   looks real" while keeping the guest runnable from just a `company_id`.

6. **DCF parameters (#45, operator-approved).** Default preset **Standard**: WACC
   10%, terminal growth 2.5% (Gordon), 5-year explicit FCF projection; operating
   assumptions tax 25%, D&A 3.5% of revenue, capex 2.5%, NWC 8%. A **Conservative**
   preset (12% / 2.0%) is selectable, and any parameter is overridable per
   request. A comparable-company **revenue-multiple cross-check** (median EV/Revenue
   over same-segment peers, segment-default fallback) accompanies the DCF.

## Consequences

- New workspace member `kanbrick-guest-sdk`; new wasm-only fixture
  `guests/sdk-example` (excluded). No toolchain change (`wasm32-wasip1` already
  pinned). The mesh build script now compiles each fixture in its own target dir.
- Security spine intact: guests still read identity only via `kbk_ctx_*`
  (host-authoritative, #23) and query only through `GuardedStore` (#24); the new
  emit/log imports carry no identity.
- The reporting guest's clearance tiers (#44) reuse the established L1–L5 model
  (Phase 2 `ClearanceScope` / Phase 4): the host already returns clearance-filtered
  rows to `query_graph`, and the guest shapes tier-appropriate output on top.
- Adding the valuation `FinancialSnapshot` data as a separate node (not Company
  properties) keeps the firm schema normalized and lets real quarterly snapshots
  replace synthetic ones later with no code change.
