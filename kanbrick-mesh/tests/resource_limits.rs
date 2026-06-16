//! #28 — memory & resource enforcement (security).
//!
//! These prove the sandbox knobs configured in ADR-0002 actually bite:
//! a per-guest linear-memory ceiling, fuel metering that kills runaway compute,
//! a locked-down WASI (no preopened filesystem), and — crucially — that the host
//! keeps serving after a guest is killed.

use kanbrick_mesh::{MeshError, MeshRuntime, RuntimeLimits};

/// Echo guest (returns input unchanged) — used to prove host recovery.
const ECHO_WAT: &str = r#"
    (module
      (memory (export "memory") 1)
      (global $next (mut i32) (i32.const 1024))
      (func (export "kbk_alloc") (param $len i32) (result i32)
        (local $p i32)
        global.get $next local.set $p
        global.get $next local.get $len i32.add global.set $next
        local.get $p)
      (func (export "kbk_run") (param $ptr i32) (param $len i32) (result i64)
        local.get $ptr i64.extend_i32_u i64.const 32 i64.shl
        local.get $len i64.extend_i32_u i64.or))
"#;

/// A guest that tries to grow linear memory by ~128 MiB, storing the result of
/// `memory.grow` (the old page count, or -1 if denied) at address 0.
const MEM_GROW_WAT: &str = r#"
    (module
      (memory (export "memory") 1)
      (func (export "kbk_alloc") (param $len i32) (result i32) (i32.const 0))
      (func (export "kbk_run") (param $ptr i32) (param $len i32) (result i64)
        (i32.store (i32.const 0) (memory.grow (i32.const 2000)))
        (i64.const 4)))
"#;

/// A guest that loops forever — only fuel exhaustion (no ticker here) stops it.
const SPIN_FOREVER_WAT: &str = r#"
    (module
      (memory (export "memory") 1)
      (func (export "kbk_alloc") (param $len i32) (result i32) (i32.const 0))
      (func (export "kbk_run") (param $ptr i32) (param $len i32) (result i64)
        (loop $l (br $l))
        (i64.const 0)))
"#;

/// A guest that probes for a preopened filesystem via `fd_prestat_get(3, ..)`,
/// storing the returned errno at address 0. A locked-down WASI has no preopens,
/// so this fails (non-zero errno) — i.e. the guest cannot reach the filesystem.
const WASI_FS_PROBE_WAT: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "fd_prestat_get"
        (func $fd_prestat_get (param i32 i32) (result i32)))
      (memory (export "memory") 1)
      (func (export "kbk_alloc") (param $len i32) (result i32) (i32.const 0))
      (func (export "kbk_run") (param $ptr i32) (param $len i32) (result i64)
        (i32.store (i32.const 0) (call $fd_prestat_get (i32.const 3) (i32.const 16)))
        (i64.const 4)))
"#;

fn read_i32(bytes: &[u8]) -> i32 {
    i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[test]
fn linear_memory_growth_is_capped_by_the_ceiling() {
    // Default 64 MiB ceiling: a 128 MiB growth request is denied (-1).
    let mut rt = MeshRuntime::new().unwrap();
    rt.register_module("grow", "0.1.0", MEM_GROW_WAT.as_bytes())
        .unwrap();
    let out = rt.dispatch("grow", b"").unwrap();
    assert_eq!(read_i32(&out), -1, "growth past the ceiling must be denied");
}

#[test]
fn fuel_metering_kills_a_runaway_guest() {
    // Default fuel budget: an infinite loop exhausts it and is killed.
    let mut rt = MeshRuntime::new().unwrap();
    rt.register_module("spin", "0.1.0", SPIN_FOREVER_WAT.as_bytes())
        .unwrap();
    let err = rt.dispatch("spin", b"").unwrap_err();
    assert!(
        matches!(err, MeshError::ResourceLimited { .. }),
        "expected ResourceLimited (fuel), got {err:?}"
    );
}

#[test]
fn wasi_has_no_filesystem_access() {
    let mut rt = MeshRuntime::new().unwrap();
    rt.register_module("probe", "0.1.0", WASI_FS_PROBE_WAT.as_bytes())
        .unwrap();
    let out = rt.dispatch("probe", b"").unwrap();
    // A non-zero errno means there is no preopened directory to enumerate: the
    // guest is sealed off from the host filesystem.
    assert_ne!(
        read_i32(&out),
        0,
        "guest must not have a preopened filesystem"
    );
}

#[test]
fn the_host_recovers_and_keeps_serving_after_a_kill() {
    let mut rt = MeshRuntime::new().unwrap();
    rt.register_module("spin", "0.1.0", SPIN_FOREVER_WAT.as_bytes())
        .unwrap();
    rt.register_module("echo", "0.1.0", ECHO_WAT.as_bytes())
        .unwrap();

    // Kill a runaway guest...
    assert!(rt.dispatch("spin", b"").is_err());
    // ...the runtime is still fully usable for the next call.
    assert_eq!(rt.dispatch("echo", b"still alive").unwrap(), b"still alive");
    // ...and killing it again then recovering again works (no corruption).
    assert!(rt.dispatch("spin", b"").is_err());
    assert_eq!(rt.dispatch("echo", b"again").unwrap(), b"again");
}

#[test]
fn a_module_whose_minimum_memory_exceeds_the_ceiling_is_rejected() {
    // A tiny 256 KiB ceiling rejects a module that declares a 1 MiB minimum.
    let limits = RuntimeLimits {
        max_memory_bytes: 256 * 1024,
        ..RuntimeLimits::default()
    };
    let big_min = r#"
        (module
          (memory (export "memory") 16)
          (func (export "kbk_alloc") (param i32) (result i32) (i32.const 0))
          (func (export "kbk_run") (param i32 i32) (result i64) (i64.const 0)))
    "#;
    let mut rt = MeshRuntime::with_limits(limits).unwrap();
    rt.register_module("big", "0.1.0", big_min.as_bytes())
        .unwrap();
    let err = rt.dispatch("big", b"").unwrap_err();
    assert!(
        matches!(err, MeshError::Instantiate { .. }),
        "expected Instantiate failure under the memory ceiling, got {err:?}"
    );
}
