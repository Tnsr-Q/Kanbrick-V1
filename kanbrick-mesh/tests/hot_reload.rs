//! #29 — hot-reload. A guest can be swapped for a new version while the runtime
//! is serving: in-flight calls drain on the old code, new calls route to the
//! replacement (nothing dropped), and a corrupt replacement is rejected while
//! the old guest keeps serving.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use kanbrick_mesh::{MeshError, MeshRuntime, RuntimeLimits};

/// v1: echoes its input.
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

/// v2: ignores input and always returns the bytes "v2".
const V2_WAT: &str = r#"
    (module
      (memory (export "memory") 1)
      (func (export "kbk_alloc") (param $len i32) (result i32) (i32.const 0))
      (func (export "kbk_run") (param $ptr i32) (param $len i32) (result i64)
        (i32.store8 (i32.const 0) (i32.const 0x76))
        (i32.store8 (i32.const 1) (i32.const 0x32))
        (i64.const 2)))
"#;

/// A guest that spins ~150M iterations and then echoes — long enough to overlap
/// with a concurrent reload, with ample fuel so it completes.
const SLOW_ECHO_WAT: &str = r#"
    (module
      (memory (export "memory") 1)
      (global $next (mut i32) (i32.const 1024))
      (func (export "kbk_alloc") (param $len i32) (result i32)
        (local $p i32)
        global.get $next local.set $p
        global.get $next local.get $len i32.add global.set $next
        local.get $p)
      (func (export "kbk_run") (param $ptr i32) (param $len i32) (result i64)
        (local $i i32)
        (block $done (loop $l
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br_if $done (i32.ge_u (local.get $i) (i32.const 150000000)))
          (br $l)))
        local.get $ptr i64.extend_i32_u i64.const 32 i64.shl
        local.get $len i64.extend_i32_u i64.or))
"#;

#[test]
fn reload_replaces_the_guest_and_updates_its_version() {
    let mut rt = MeshRuntime::new().unwrap();
    rt.register_module("g", "1.0.0", ECHO_WAT.as_bytes())
        .unwrap();
    assert_eq!(rt.dispatch("g", b"hello").unwrap(), b"hello");

    rt.reload_module("g", "2.0.0", V2_WAT.as_bytes()).unwrap();
    assert_eq!(rt.dispatch("g", b"hello").unwrap(), b"v2");

    let info = rt.guests();
    assert_eq!(info.len(), 1);
    assert_eq!(info[0].version, "2.0.0");
}

#[test]
fn a_corrupt_replacement_is_rejected_and_the_old_guest_keeps_serving() {
    let mut rt = MeshRuntime::new().unwrap();
    rt.register_module("g", "1.0.0", ECHO_WAT.as_bytes())
        .unwrap();

    let err = rt
        .reload_module("g", "2.0.0", b"\0not a wasm module")
        .unwrap_err();
    assert!(matches!(err, MeshError::Compile { .. }));

    // The old guest is untouched: still serving, still v1.
    assert_eq!(rt.dispatch("g", b"still here").unwrap(), b"still here");
    assert_eq!(rt.guests()[0].version, "1.0.0");
}

#[test]
fn an_in_flight_call_drains_on_the_old_module_while_new_calls_get_the_replacement() {
    // Ample fuel so the slow guest completes rather than being fuel-killed.
    let limits = RuntimeLimits {
        fuel: u64::MAX,
        ..RuntimeLimits::default()
    };
    let mut rt = MeshRuntime::with_limits(limits).unwrap();
    rt.register_module("g", "1.0.0", SLOW_ECHO_WAT.as_bytes())
        .unwrap();
    let rt = Arc::new(rt);

    // Start a long call on the old (echo) version.
    let in_flight = {
        let rt = rt.clone();
        thread::spawn(move || rt.dispatch("g", b"in-flight").unwrap())
    };

    // While it runs, hot-reload the guest to v2.
    thread::sleep(Duration::from_millis(20));
    rt.reload_module("g", "2.0.0", V2_WAT.as_bytes()).unwrap();

    // The in-flight call drained on the OLD module: it echoes, not "v2".
    assert_eq!(in_flight.join().unwrap(), b"in-flight");
    // New calls route to the replacement.
    assert_eq!(rt.dispatch("g", b"anything").unwrap(), b"v2");
}
