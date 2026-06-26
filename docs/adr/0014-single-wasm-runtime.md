# ADR 0014 — Single WASM runtime: `kanbrick-mesh` (wasmtime 45 / WASIp1) stays the only guest runtime

- **Status:** Accepted
- **Date:** 2026-06-25
- **Context:** P8.1 (#93), the first slice of **Phase 8 — Upstream De-Risk**
  (#79), part of the L5 Cockpit program (#77). Ratifies and extends ADR-0002
  *before* Phase 8 pulls bounded primitives **from** the same upstream.
- **Deciders:** P8 de-risk agent + operator (HITL — single-vs-dual runtime is a
  one-way door).

## Context

Phase 8 deliberately mines Tachyon-Mesh for **bounded primitives** — IOTA
Stronghold as a secret enclave (ADR-0009) and its MCP server as a loop tool-call
bridge (the P8.3 probe note). Before taking any of them we must re-nail the
load-bearing decision from ADR-0002: that we take *primitives*, never the
**runtime**. Otherwise a primitive could quietly drag in `core-host` and we would
inherit a second guest runtime, a second WASM ABI, and a toolchain bump.

ADR-0002 already established (Phase 3, run 2026-06-15, `core-host` @ `31d63b9`):

- `kanbrick-mesh` is built **directly on wasmtime 45** with **WASIp1** guests and
  our own minimal host↔guest ABI (JSON boundary).
- Tachyon-Mesh `core-host` (~50K LOC / 76 files) is a Component-Model /
  `wasm32-wasip2` host carrying eBPF (`aya`) and AI inference (`candle`/ONNX); it
  pins **rustc 1.96.0** and **wasmtime 45.0.0**, and is kept as a **reference
  only**, never a dependency.
- Phase 2 separately found **Ironclaw is a binary** (no library target) and built
  `kanbrick-auth` on its *primitives* (`jsonwebtoken` + `argon2`).

This ADR re-confirms that doctrine as the explicit gate for every Phase-8 probe.

## Probe evidence

The slice's task was to `git submodule update --init crates/ironclaw
crates/tachyon-mesh`, build each in isolation, and record the matrix. **Honest
environment note:** in this de-risk session the submodule clone returns **HTTP
403** from the agent proxy — only `tnsr-q/kanbrick-v1` is in network scope, so
`JoasASantos/ironclaw` and `astorise/Tachyon-Mesh` cannot be cloned here. The
matrix below is therefore reconstructed from ADR-0002's recorded probe and the
Phase-2 Ironclaw finding, and is reproduced on a submodule-capable machine / CI.
Full notes: [`docs/probes/p8.1-upstream-compat-matrix.md`](../probes/p8.1-upstream-compat-matrix.md).

| Component | Role | Runtime / ABI | wasmtime | WASI / target | toolchain | In our build graph? |
| --- | --- | --- | --- | --- | --- | --- |
| `kanbrick-mesh` | **the** guest runtime | own minimal ABI | 45 | WASIp1 / `wasm32-wasip1` | 1.94.1 | yes |
| tachyon-mesh `core-host` | reference only | Component Model + WIT | 45.0.0 | WASIp2 / `wasm32-wasip2` | 1.96.0 | **no** |
| ironclaw | primitives only (Phase 2) | n/a — binary | — | n/a | ≥ 1.75 | **no** |
| our pins | — | — | 45 | `wasm32-wasip1` | 1.94.1 | — |

## Decision

1. **`kanbrick-mesh` (wasmtime 45, WASIp1) remains the only guest runtime.** No
   second runtime is introduced in Phase 8 or by anything it pulls in.
2. **"Bounded primitive, never the runtime."** A Phase-8 probe may adopt a
   *library* primitive from Tachyon-Mesh (e.g. Stronghold, ADR-0009) only if it is
   usable **without** `core-host`; if a primitive can only come via `core-host` it
   is rejected or re-implemented. The MCP server is taken as an **out-of-process
   sidecar**, not linked (P8.3 probe note).
3. **No toolchain bump and no `wasm32-wasip2` / Component Model adoption.** Bumping
   to rustc 1.96.0 would force re-verification of SparrowDB / graphify and the
   whole workspace; WASIp2 + eBPF + `candle`/ONNX are out of scope and conflict
   with the no-network ethos (ADR-0003/0006).

## Alternatives considered

- **Adopt `core-host` (Path A from ADR-0002).** Rejected again: a second WASM ABI,
  a 1.96.0 toolchain bump, and eBPF / AI-infra coupling, for no Phase-8
  requirement.
- **Dual runtime (mesh for guests, core-host for AI).** Rejected: two ABIs and two
  security-review surfaces; the BYO-AI need is served by an egress boundary
  (ADR-0017), not an in-process inference host.
- **Re-clone & rebuild the upstreams here for a fresh matrix.** Not possible in
  this environment (403); deferred to CI / an operator machine. The decision does
  not depend on it — ADR-0002's evidence is decisive.

## Consequences

- The workspace build is unaffected: `crates/ironclaw` and `crates/tachyon-mesh`
  stay in the root `Cargo.toml` `exclude` set (alongside `crates/sparrowdb`,
  `crates/sparrowdb-ontology`, the `guests/*` wasm fixtures, `cockpit/src-tauri`,
  and the new `probes/*`).
- Every downstream Phase-8 probe inherits the gate: prove the primitive works
  **without** `core-host` (ADR-0009 does this empirically; the P8.3 note keeps MCP
  out-of-process).
- A future graduation to the Component Model remains a *later* ADR if a real need
  appears; this ADR is the current one-way-door record.
