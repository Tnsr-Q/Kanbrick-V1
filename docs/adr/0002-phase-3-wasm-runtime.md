# ADR 0002 — Phase 3 WASM runtime: build `kanbrick-mesh` on wasmtime 45

- **Status:** Accepted
- **Date:** 2026-06-15
- **Context:** Phase 3 (orchestration) — issues #21–#29. Informs the guest SDK
  (#39) and all WASM guests (Phase 5).
- **Deciders:** Phase 3 agent + operator (HITL: #21 runtime config, #22 ABI are
  one-way doors).
- **Pinned upstream studied:** `crates/tachyon-mesh` @ `31d63b9` (`core-host` 1.0.0).

## Context

The PRD names **Tachyon-Mesh** as the orchestration/WASM engine and lists a
separate `kanbrick-wasm-host` crate. Before committing code we de-risked the
vendored upstream the same way Phase 1 (SparrowDB dialect) and Phase 2 (Ironclaw
"is a binary") did — with throwaway probes — and brought the architectural fork
to the operator.

### What Tachyon-Mesh actually is

`crates/tachyon-mesh` is **not a drop-in library**. It is a multi-crate
workspace; the WASM host lives in `core-host` (~50K LOC / 76 files): mesh control
plane, leader election, **eBPF (`aya`)**, **AI inference (`candle`/ONNX)**,
telemetry, UDS fast-path. Its host↔guest ABI is **WASM Component Model + WIT**
targeting **`wasm32-wasip2`**. It pins **rustc 1.96.0** and **wasmtime 45.0.0**.

### Probe evidence (run 2026-06-15)

| Probe | Command | Result |
| --- | --- | --- |
| **Path A** — depend on `core-host` | `cargo build -p core-host --no-default-features` (its pinned 1.96.0) | **Builds OK**, ~4m45s. Feasible, but on a *different toolchain* than our workspace (1.94.1) and only with default-features off. |
| **Path B** — build on wasmtime | scratch crate, `wasmtime` + `wasmtime-wasi` = `"45"` on our pinned **1.94.1** | **Builds + runs.** `wasmtime 45.0.2`. Echo module instantiated and called (`id(7) → 7`). All Phase-3 knobs present: `Config::consume_fuel`, `Config::epoch_interruption`, `StoreLimitsBuilder::memory_size`, `Store::set_fuel`, `Store::set_epoch_deadline`. WASIp1 lockdown via `WasiCtxBuilder::new().build_p1()` (no fs/net by default) + `p1::add_to_linker_sync`. |

## Decision

1. **Path B — build `kanbrick-mesh` directly on `wasmtime` 45** with **WASIp1**
   guests and **our own minimal Host-Guest ABI** (ABI types in `kanbrick-core`,
   per #22). Tachyon-Mesh `core-host` is kept as a *reference* for ABI/WIT design
   and a future graduation target, **not** a dependency.

   *Rationale:* zero toolchain bump (stays on 1.94.1 — no re-verification of
   SparrowDB/graphify/workspace), no `wasip2`, no eBPF/AI infra coupling, no CI
   submodule requirement; fully in our control and testable. wasmtime is the
   exact primitive Tachyon-Mesh itself uses. This mirrors Phase 2 building
   `kanbrick-auth` on Ironclaw's *primitives* rather than the binary, and matches
   the PRD risk register ("Pin version; wrap ABI in stable kanbrick types").

2. **Fold the WASM host into `kanbrick-mesh`.** **PRD deviation, flagged:** the
   PRD crate inventory names a separate `kanbrick-wasm-host` crate. It does not
   exist in the workspace, and for a single in-process runtime a second crate
   adds boundaries without value. `kanbrick-mesh` owns the engine, module
   registry, dispatch, scheduler, and event bus. Revisit only if a second host
   embedding appears.

3. **Boundary serialization format = JSON** (#22). `FirmContext`, `ClearanceLevel`,
   and query results are already `serde` types that round-trip through JSON
   (verified in Phase 2 tests). JSON over the linear-memory boundary is the
   low-risk default; bytes in / bytes out at the raw call, typed payloads
   serialized as JSON. Revisit if profiling shows the boundary is hot (a compact
   binary codec like `postcard` is a drop-in later).

4. **Runtime limits / config defaults (#21, HITL — operator-approved).** The
   `Engine` enables `consume_fuel` and `epoch_interruption` so #25/#28 can use
   them; each guest `Store` is locked down at instantiation:

   | Knob | Default | Enforced by | Issue |
   | --- | --- | --- | --- |
   | Max linear memory | **64 MiB** | `StoreLimits` (`memory_size`) | #28 |
   | Fuel per dispatch | **1e9 units** | `Store::set_fuel` | #28 |
   | Wall-clock timeout | **5 s** (epoch ticker) | `epoch_interruption` | #25 |
   | Filesystem access | **none** | `WasiCtxBuilder` default (no preopens) | #28 |
   | Network access | **none** | `WasiCtxBuilder` default (no `inherit_network`) | #28 |
   | stdio | **none inherited** | `WasiCtxBuilder` default | #28 |

   In #21 the limits struct exists and the memory ceiling + fuel are applied; the
   *kill* paths (fuel exhaustion, epoch timeout) are exercised by #25/#28. The
   wall-clock value backs the PRD "slow guest killed at its TTL" checkpoint.

## The host↔guest calling convention (minimal, formalized in #22)

WASIp1 reactor guests (`crate-type = ["cdylib"]`) export `memory` plus:

- `kbk_alloc(len: u32) -> u32` — reserve `len` bytes, return the offset.
- `kbk_run(ptr: u32, len: u32) -> u64` — process input bytes at `[ptr, ptr+len)`;
  return a packed `(out_ptr << 32) | out_len`; output bytes live in guest memory.

The host writes input via `kbk_alloc`, calls `kbk_run`, and reads the packed
output. `_initialize` is called once after instantiation if exported. This is the
substrate the #22 `HostFunctions`/`GuestModule` traits and the #39 guest SDK
build typed JSON payloads on top of.

## Consequences

- `wasmtime = "45"` (already pinned) gains a real consumer; **`wasmtime-wasi = "45"`**
  is added to `[workspace.dependencies]`. No toolchain or CI submodule change.
- A new **`guests/echo`** crate (excluded from the workspace; built to
  `wasm32-wasip1` by `kanbrick-mesh`'s build script) is the first real guest and
  the test fixture for #21–#24.
- `kanbrick-wasm-host` is intentionally **not** created (deviation noted above).
- Security spine unchanged: `query_graph` (#24) routes through
  `kanbrick_auth::GuardedStore`; `FirmContext` stays host-authoritative (#23).
- Revisit this ADR if we later need Component Model/`wasip2`, multi-host
  embeddings, or Tachyon-Mesh's mesh/eBPF/AI features — at which point Path A (or
  a hybrid) is reconsidered with `core-host` as the on-ramp.
