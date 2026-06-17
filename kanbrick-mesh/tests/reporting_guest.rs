//! #43/#44 end-to-end: the reporting guest, compiled to real `wasm32-wasip1`,
//! driven through the mesh. The same dashboard request yields tier-appropriate
//! output: a public roster for all, detail gated by clearance (ADR-0005).

use std::sync::Arc;

use kanbrick_core::abi::GuestRequest;
use kanbrick_core::{ClearanceLevel, FirmContext};
use kanbrick_mesh::MeshRuntime;
use kanbrick_store::{Migrator, Store};
use serde_json::json;
use uuid::Uuid;

const REPORTING_WASM: &str = env!("KANBRICK_REPORTING_GUEST_WASM");

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
    let wasm = std::fs::read(REPORTING_WASM).expect("read reporting wasm");
    let mut rt = MeshRuntime::new().unwrap().with_store(store);
    rt.register_module("reporting", "0.1.0", &wasm).unwrap();
    rt
}

fn dashboard(rt: &MeshRuntime, email: &str, clearance: ClearanceLevel) -> serde_json::Value {
    let ctx = FirmContext::new(Uuid::new_v4(), email, clearance);
    rt.invoke(
        "reporting",
        &ctx,
        &GuestRequest::new(json!({"report": "portfolio_dashboard"})),
    )
    .unwrap()
    .payload
}

fn detail_count(dash: &serde_json::Value) -> usize {
    dash["companies"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c.get("detail").is_some())
        .count()
}

#[test]
fn l5_dashboard_has_all_companies_in_detail() {
    let (_d, store) = seeded();
    let rt = runtime(store);
    let dash = dashboard(&rt, "tracy.brittcool@kanbrick.com", ClearanceLevel::L5);

    let companies = dash["companies"].as_array().unwrap();
    assert_eq!(companies.len(), 9);
    assert_eq!(detail_count(&dash), 9);
    assert_eq!(dash["totals"]["company_count"], json!(9));
    assert_eq!(dash["totals"]["segment_count"], json!(4));
    assert_eq!(dash["totals"]["headcount"], json!(12));

    // JMTS shows its five stakeholders to the L5 viewer.
    let jmts = companies
        .iter()
        .find(|c| c["company_id"] == json!("JMTS"))
        .unwrap();
    assert_eq!(jmts["detail"]["stakeholder_count"], json!(5));
}

#[test]
fn l1_sees_public_roster_only() {
    let (_d, store) = seeded();
    let rt = runtime(store);
    let dash = dashboard(&rt, "dana.prescott@kanbrick.com", ClearanceLevel::L1);

    // Full public roster, zero detail, headcount = just themselves.
    assert_eq!(dash["companies"].as_array().unwrap().len(), 9);
    assert_eq!(detail_count(&dash), 0);
    assert_eq!(dash["totals"]["company_count"], json!(9));
    assert_eq!(dash["totals"]["headcount"], json!(1));
    // Company names are present (public).
    let jmts = dash["companies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["company_id"] == json!("JMTS"))
        .unwrap();
    assert_eq!(jmts["name"], json!("JM Test Systems"));
}

#[test]
fn l3_sees_roster_for_all_detail_for_own_segment() {
    let (_d, store) = seeded();
    let rt = runtime(store);
    let dash = dashboard(&rt, "tyler.begemann@kanbrick.com", ClearanceLevel::L3);

    assert_eq!(
        dash["companies"].as_array().unwrap().len(),
        9,
        "roster public"
    );
    assert_eq!(
        detail_count(&dash),
        5,
        "detail only for the 5 segment companies"
    );

    // The same report at L5 vs L3 yields verifiably different output.
    let l5 = dashboard(&rt, "tracy.brittcool@kanbrick.com", ClearanceLevel::L5);
    assert_ne!(detail_count(&dash), detail_count(&l5));
}
