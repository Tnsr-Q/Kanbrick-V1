//! #45 end-to-end: the valuation guest, compiled to real `wasm32-wasip1`, driven
//! through the mesh against the seed graph plus the synthetic financials dataset.

use std::sync::Arc;

use kanbrick_core::abi::GuestRequest;
use kanbrick_core::{ClearanceLevel, FirmContext};
use kanbrick_mesh::MeshRuntime;
use kanbrick_store::{seed, Migrator, Store};
use serde_json::json;
use uuid::Uuid;

const VALUATION_WASM: &str = env!("KANBRICK_VALUATION_GUEST_WASM");

/// Seed the firm graph plus the optional synthetic financials.
fn seeded() -> (tempfile::TempDir, Arc<Store>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let firm = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../seed/kanbrick_seed_data.cypher"
    ))
    .unwrap();
    Migrator::firm(firm).run(&store).unwrap();
    let financials = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../seed/kanbrick_financials.cypher"
    ))
    .unwrap();
    seed::load_str(&store, &financials).unwrap();
    (dir, Arc::new(store))
}

fn runtime(store: Arc<Store>) -> MeshRuntime {
    let wasm = std::fs::read(VALUATION_WASM).expect("read valuation wasm");
    let mut rt = MeshRuntime::new().unwrap().with_store(store);
    rt.register_module("valuation", "0.1.0", &wasm).unwrap();
    rt
}

#[test]
fn dcf_on_jm_test_systems_produces_a_report() {
    let (_d, store) = seeded();
    let rt = runtime(store);

    // PRD checkpoint: DCF on JM Test Systems → structured ValuationReport. Run as
    // the CEO (L5) so the company's financials are visible.
    let ceo = FirmContext::new(
        Uuid::new_v4(),
        "tracy.brittcool@kanbrick.com",
        ClearanceLevel::L5,
    );
    let report = rt
        .invoke(
            "valuation",
            &ceo,
            &GuestRequest::new(json!({"company_id": "JMTS"})),
        )
        .unwrap()
        .payload;

    assert_eq!(report["company_id"], json!("JMTS"));
    assert_eq!(report["company_name"], json!("JM Test Systems"));
    assert!(report["enterprise_value"].as_f64().unwrap() > 0.0);
    assert!(report["equity_value"].as_f64().unwrap() > 0.0);
    assert!(report["revenue_multiple_valuation"].as_f64().unwrap() > 0.0);
    // It valued off the graph's synthetic snapshot, and says so.
    assert_eq!(
        report["financials_source"]["source"],
        json!("graph_default")
    );
    assert_eq!(
        report["financials_source"]["source_tag"],
        json!("SYNTHETIC")
    );
    assert!(report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w.as_str().unwrap().contains("SYNTHETIC")));
}

#[test]
fn an_l2_caller_is_rejected() {
    let (_d, store) = seeded();
    let rt = runtime(store);

    // Valuation requires L3+; an L2 analyst is rejected with a structured error.
    let l2 = FirmContext::new(
        Uuid::new_v4(),
        "elena.ruiz@kanbrick.com",
        ClearanceLevel::L2,
    );
    let resp = rt
        .invoke(
            "valuation",
            &l2,
            &GuestRequest::new(json!({"company_id": "JMTS"})),
        )
        .unwrap()
        .payload;
    assert_eq!(resp["kind"], json!("Unauthorized"));
    assert!(resp["error"].as_str().unwrap().contains("clearance L3"));
}

#[test]
fn payload_financials_override_the_graph() {
    let (_d, store) = seeded();
    let rt = runtime(store);

    // A caller supplies their own figures (authority); the report reflects the
    // user-provided source and carries higher confidence with no synthetic warning.
    let lead = FirmContext::new(
        Uuid::new_v4(),
        "tyler.begemann@kanbrick.com", // L3 lead who can see JMTS (his segment)
        ClearanceLevel::L3,
    );
    let report = rt
        .invoke(
            "valuation",
            &lead,
            &GuestRequest::new(json!({
                "company_id": "JMTS",
                "financials": {
                    "revenue": 30000000.0,
                    "ebitda": 5000000.0,
                    "fcf": 3500000.0,
                    "growth_rate": 0.10,
                    "net_debt": 7000000.0
                },
                "financials_note": "Q2-2026 board deck",
                "scenario": {"preset": "conservative"}
            })),
        )
        .unwrap()
        .payload;

    assert_eq!(
        report["financials_source"]["source"],
        json!("user_provided")
    );
    assert_eq!(
        report["financials_source"]["note"],
        json!("Q2-2026 board deck")
    );
    assert_eq!(report["parameters"]["preset"], json!("conservative"));
    assert!(report["confidence"].as_f64().unwrap() > 0.8);
    assert!(report["warnings"].as_array().unwrap().is_empty());
}
