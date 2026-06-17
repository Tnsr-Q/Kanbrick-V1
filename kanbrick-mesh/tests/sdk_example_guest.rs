//! #39 end-to-end: the guest-SDK reference guest, compiled to real
//! `wasm32-wasip1`, driven through the mesh. Proves the SDK's `firm_context`,
//! `query_graph` (clearance-filtered), `emit`, and `log` work over the wasm
//! boundary — auth → mesh → guest → store.

use std::sync::Arc;

use kanbrick_core::abi::GuestRequest;
use kanbrick_core::{ClearanceLevel, FirmContext};
use kanbrick_mesh::{EventBus, MeshRuntime};
use kanbrick_store::{Migrator, Store};
use serde_json::json;
use uuid::Uuid;

/// The SDK example guest, compiled to wasm by this crate's build script.
const SDK_EXAMPLE_WASM: &str = env!("KANBRICK_SDK_EXAMPLE_GUEST_WASM");

fn seeded() -> (tempfile::TempDir, Arc<Store>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let seed = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../seed/kanbrick_seed_data.cypher"
    ))
    .unwrap();
    Migrator::firm(seed).run(&store).unwrap();
    (dir, Arc::new(store))
}

fn runtime(store: Arc<Store>, bus: EventBus) -> MeshRuntime {
    let wasm = std::fs::read(SDK_EXAMPLE_WASM).expect("read sdk-example wasm");
    let mut rt = MeshRuntime::new().unwrap().with_store(store).with_bus(bus);
    rt.register_module("sdk-example", "0.1.0", &wasm).unwrap();
    rt
}

#[test]
fn sdk_guest_reads_context_queries_and_emits_end_to_end() {
    let (_d, store) = seeded();
    let bus = EventBus::new();
    let rt = runtime(store, bus.clone());

    // An L3 segment lead: query_graph is clearance-filtered to their 5 companies.
    let l3 = FirmContext::new(
        Uuid::new_v4(),
        "tyler.begemann@kanbrick.com",
        ClearanceLevel::L3,
    );
    let resp = rt
        .invoke("sdk-example", &l3, &GuestRequest::new(json!({})))
        .unwrap();

    assert_eq!(resp.payload["caller"], "tyler.begemann@kanbrick.com");
    assert_eq!(resp.payload["clearance"], "L3");
    assert_eq!(resp.payload["row_count"], 5);

    // The guest emitted its completion event onto the host bus.
    let history = bus.history();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].kind, "example.completed");
    assert_eq!(history[0].payload["row_count"], 5);
}

#[test]
fn sdk_guest_sees_clearance_specific_rows() {
    let (_d, store) = seeded();
    let bus = EventBus::new();
    let rt = runtime(store, bus);

    // The same guest, same request: the L5 CEO sees all 9 companies. The guest
    // cannot influence which identity the host injects.
    let l5 = FirmContext::new(
        Uuid::new_v4(),
        "tracy.brittcool@kanbrick.com",
        ClearanceLevel::L5,
    );
    let resp = rt
        .invoke("sdk-example", &l5, &GuestRequest::new(json!({})))
        .unwrap();
    assert_eq!(resp.payload["clearance"], "L5");
    assert_eq!(resp.payload["row_count"], 9);
}
