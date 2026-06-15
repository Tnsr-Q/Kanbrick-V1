//! End-to-end test for issue #21 against the **real** `wasm32-wasip1` echo
//! guest (`guests/echo`), compiled by this crate's build script.
//!
//! The unit tests in `runtime.rs` cover the dispatch machinery with a hermetic
//! WAT module; this test proves the actual Rust→WASM guest loads, registers, and
//! round-trips bytes through the WASIp1 sandbox.

use kanbrick_core::abi::GuestRequest;
use kanbrick_core::{ClearanceLevel, FirmContext};
use kanbrick_mesh::MeshRuntime;
use uuid::Uuid;

/// The echo guest, compiled to `wasm32-wasip1` and located by `build.rs`.
const ECHO_WASM: &[u8] = include_bytes!(env!("KANBRICK_ECHO_GUEST_WASM"));

#[test]
fn echo_guest_loads_registers_and_round_trips() {
    let mut rt = MeshRuntime::new().expect("runtime init");

    // The mesh layer loads the echo guest module at startup...
    rt.register_module("echo", "0.1.0", ECHO_WASM)
        .expect("register echo guest");

    // ...and it appears in the registry with a name and version.
    let guests = rt.guests();
    assert_eq!(guests.len(), 1);
    assert_eq!(guests[0].name, "echo");
    assert_eq!(guests[0].version, "0.1.0");
    assert!(rt.contains("echo"));

    // Dispatching to the echo guest returns the identical input bytes.
    let input = b"hello kanbrick mesh";
    let output = rt.dispatch("echo", input).expect("dispatch echo");
    assert_eq!(output, input, "echo guest must return input unchanged");

    // Edge cases: empty input and binary bytes (including a NUL).
    assert_eq!(rt.dispatch("echo", b"").unwrap(), b"");
    let binary: Vec<u8> = (0u8..=255).collect();
    assert_eq!(rt.dispatch("echo", &binary).unwrap(), binary);
}

#[test]
fn dispatch_to_unregistered_guest_is_an_error() {
    let rt = MeshRuntime::new().unwrap();
    assert!(rt.dispatch("echo", b"x").is_err());
}

#[test]
fn invoke_round_trips_a_typed_request_through_the_real_guest() {
    // #22/#23 over the real wasm32-wasip1 boundary: a typed GuestRequest is
    // JSON-encoded, dispatched on behalf of a host-authoritative FirmContext,
    // and decoded back into a GuestResponse. The echo guest returns its input
    // unchanged, so the response payload matches the request payload.
    let mut rt = MeshRuntime::new().unwrap();
    rt.register_module("echo", "0.1.0", ECHO_WASM).unwrap();

    let ctx = FirmContext::new(Uuid::new_v4(), "analyst@kanbrick.com", ClearanceLevel::L3);
    let request = GuestRequest::new(serde_json::json!({"company_id": 42, "year": 2026}));
    let response = rt.invoke("echo", &ctx, &request).unwrap();

    assert_eq!(response.payload, request.payload);
}
