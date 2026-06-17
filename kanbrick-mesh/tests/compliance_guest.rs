//! #41/#42 end-to-end: the compliance guest, compiled to real `wasm32-wasip1`,
//! driven through the mesh against the seed graph (auth → mesh → guest → store).

use std::sync::Arc;

use kanbrick_core::abi::GuestRequest;
use kanbrick_core::{ClearanceLevel, FirmContext};
use kanbrick_mesh::MeshRuntime;
use kanbrick_store::{Migrator, Store};
use serde_json::json;
use uuid::Uuid;

const COMPLIANCE_WASM: &str = env!("KANBRICK_COMPLIANCE_GUEST_WASM");

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

fn runtime(store: Arc<Store>) -> MeshRuntime {
    let wasm = std::fs::read(COMPLIANCE_WASM).expect("read compliance wasm");
    let mut rt = MeshRuntime::new().unwrap().with_store(store);
    rt.register_module("compliance", "0.1.0", &wasm).unwrap();
    rt
}

#[test]
fn compliance_passes_on_seed_for_l5() {
    let (_d, store) = seeded();
    let rt = runtime(store);

    let l5 = FirmContext::new(
        Uuid::new_v4(),
        "tracy.brittcool@kanbrick.com",
        ClearanceLevel::L5,
    );
    let resp = rt
        .invoke(
            "compliance",
            &l5,
            &GuestRequest::new(json!({"check": "all"})),
        )
        .unwrap();

    // The seed graph is compliant — same result the native logic test asserts.
    assert_eq!(resp.payload["passed"], json!(true));
    assert!(resp.payload["violations"].as_array().unwrap().is_empty());
    assert_eq!(
        resp.payload["checks_run"],
        json!(["org_chart", "clearance"])
    );
}

#[test]
fn compliance_rejects_an_l3_caller() {
    let (_d, store) = seeded();
    let rt = runtime(store);

    // The check requires L4+; an L3 segment lead is rejected with a structured
    // error (not a panic/trap).
    let l3 = FirmContext::new(
        Uuid::new_v4(),
        "tyler.begemann@kanbrick.com",
        ClearanceLevel::L3,
    );
    let resp = rt
        .invoke(
            "compliance",
            &l3,
            &GuestRequest::new(json!({"check": "all"})),
        )
        .unwrap();

    assert_eq!(resp.payload["kind"], json!("Unauthorized"));
    assert!(resp.payload["error"]
        .as_str()
        .unwrap()
        .contains("clearance L4"));
}
