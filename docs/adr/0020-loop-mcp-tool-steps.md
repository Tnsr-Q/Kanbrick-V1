# ADR 0020 — Loop MCP tool-call steps: managed sidecar, capability passthrough, injected seam

- **Status:** Accepted
- **Date:** 2026-06-29
- **Context:** Phase 11 (Skill/Loop Ecosystem), slice **P11.5** — external MCP
  tool-call steps for the loop run engine (ADR-0013). Builds on the **P8.3 probe**
  (`docs/probes/p8.3-mcp-bridge.md`, wrap-vs-sidecar = sidecar), ADR-0008 (per-invocation
  capability model), ADR-0014 (single WASM runtime — no `core-host` coupling), ADR-0016
  (host-authoritative IPC identity), ADR-0017 (core no-egress), and the P7.2
  `SidecarSupervisor` + the `InvocationCaps` / internal-RPC substrate already in
  `kanbrick-api`. Sits beside ADR-0019 (provider steps): the two together make the loop
  step polymorphic across **three** kinds.
- **Deciders:** P11 agent (the substrate + locked probe decision; AFK slice).

## Context

P11.3 shipped the loop run engine: a `(:LoopStep)` is `(skill_name, scope_id)`,
authorized at run time by `ScopeGrants::authorize_skill`, then run as a WASM guest on
the mesh `Scheduler`. P11.4 (ADR-0019) made the step polymorphic — a step may instead
run an **LLM completion**. P11.5 adds the **third kind**: an **external MCP tool-call**.

The load-bearing question is the same one ADR-0019 answered for keys, now for tools:
*how does a step name an external tool without the core gaining egress, a second
runtime, or the ability to act as another identity?* The P8.3 probe locked the shape:
wrap the upstream `tachyon-mcp` server as a **managed sidecar** (the P7.2 spawn →
`/health`-gate → supervise → kill pattern), **not** a `kanbrick-mesh::HostServices`
backend — that trait is the guest↔host *graph* ABI (`query_graph`/`emit_event`), a
different concern from calling external tools. Identity passes through as a
per-invocation **capability** (ADR-0008): the host mints a cap bound to the caller's
`FirmContext`, the sidecar receives only the opaque cap + the authorized tool + args,
and the host re-enters server-side to resolve the cap for any callback.

## Decision

1. **A third, skill-bound step kind.** An MCP tool step is `(skill_name, scope_id,
   tool_ref)` and goes through the **same** `authorize_skill` gate as guest and
   provider steps — active+unexpired scope, caller is the grantee, clearance ≥ the
   skill's floor. `tool_ref` (a tool name + optional static args) **overrides
   execution** ("call this external tool instead of running the bound guest") but never
   the gate. The skill supplies authorization + the `ProjectScope`; one uniform run
   gate covers all three kinds. The schema stores `tool`/`tool_args` as **opaque
   strings** in `(:LoopStep)`, so `kanbrick-store` stays free of any MCP dependency;
   the run engine resolves the kind (tool > provider > guest), parses, and dispatches.
   The create route rejects a step that sets more than one kind (400).

2. **Capability passthrough; the step never names an identity.** At run time the host
   mints a per-invocation capability (`InvocationCaps::mint`) bound to the run's
   **caller** `FirmContext` (the host-authoritative identity, ADR-0002/0016), hands the
   bridge **only** the opaque cap + the tool + the args the scope authorizes, and
   **revokes the cap** the moment the call returns (mirroring `invoke_guest`'s executor
   cap). The args derive from the step's static `tool_args` (validated to be a JSON
   object) merged with the piped payload under `"input"`. A step can neither carry a
   credential nor inject an identity — identity rides the cap opaquely, host-side, and
   only the host can resolve it. The host (not the sidecar) applies the returned result
   by piping it onward.

3. **Managed sidecar, not a HostServices backend; no second runtime.** The real bridge
   wraps `tachyon-mcp` as a managed sidecar (P7.2 `SidecarSupervisor`: spawn →
   `/health`-gate → supervise → kill) and relays over the
   `x-kanbrick-internal-token`-gated control channel (the same fail-closed gate as the
   internal RPC surface, `kanbrick-api/src/internal.rs`). No `core-host` coupling, no
   second WASM runtime (ADR-0014).

4. **Seam-only, no live subprocess/socket (this slice).** The tool is called through an
   injected `McpBridge` seam on `AppState` (`with_mcp_bridge`), exactly mirroring the
   ADR-0019 `ProviderFactory`. The default is a **no-network stub** (canned echo); **no
   subprocess or socket ships in core/CI**, as ADR-0017 / P9.4 / P9.6 / P11.4 require —
   and because the `tachyon-mesh` submodule is proxy-blocked (HTTP 403, probes
   P8.1/P8.3) with no local Rust compile. At deploy the real managed-sidecar bridge is
   injected; the security property P11.5 owns (host-minted caller-bound cap, tool-only
   selection, core no-egress) is fully wired and tested with zero network.

## Consequences

- **The security invariant is the deliverable and is verified.** A recording-bridge
  integration test asserts the bridge received an **opaque cap that resolves host-side
  to the caller** (proving identity stayed host-side and the step named only the tool),
  the tool, and the args (static args + the piped payload) — plus a bridge-error step
  fails the run, an empty/duplicate-kind step is rejected at create (400), and the cap
  is revoked after the call (unit-tested in `caps.rs`). Guest and provider steps are
  unchanged.
- **Egress + runtime stay compliant.** The core run engine opens no socket and spawns
  no process; the live `tachyon-mcp` sidecar (and its supervision + internal-token
  channel) is the injected deploy-time bridge's job. ADR-0014 holds — no second WASM
  runtime, no `HostServices` coupling.
- **`HostServices` reconsidered only if a tool needs in-process clearance-filtered
  graph access** (probe P8.3's escape hatch); nothing here introduces it.
- **Deferred:** the live managed-sidecar bridge impl + a live tool-call (submodule- /
  network-gated); a richer per-tool argument schema beyond the opaque `tool_args`
  object; durable run history (a separate P11.5 companion); the cockpit UI for
  authoring/labelling MCP tool steps (P11.6, which surfaces `tool_ref {tool}` once this
  lands). Token accounting for any priced tool stays Phase 12.
