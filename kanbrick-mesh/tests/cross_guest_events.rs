//! #46 end-to-end: a completed valuation emits `valuation.completed` onto the
//! host bus; a subscriber drives the reporting guest to regenerate its dashboard.
//! Both guests are real `wasm32-wasip1` modules driven through the mesh, and the
//! emit happens re-entrantly from inside the valuation guest's host call.

use std::sync::{Arc, Mutex};

use kanbrick_core::abi::GuestRequest;
use kanbrick_core::{ClearanceLevel, FirmContext};
use kanbrick_mesh::{EventBus, MeshRuntime};
use kanbrick_store::{seed, Migrator, Store};
use serde_json::json;
use uuid::Uuid;

const VALUATION_WASM: &str = env!("KANBRICK_VALUATION_GUEST_WASM");
const REPORTING_WASM: &str = env!("KANBRICK_REPORTING_GUEST_WASM");

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

/// A runtime with both guests registered and a bus bound.
fn runtime(store: Arc<Store>, bus: EventBus) -> Arc<MeshRuntime> {
    let mut rt = MeshRuntime::new().unwrap().with_store(store).with_bus(bus);
    rt.register_module(
        "valuation",
        "0.1.0",
        &std::fs::read(VALUATION_WASM).unwrap(),
    )
    .unwrap();
    rt.register_module(
        "reporting",
        "0.1.0",
        &std::fs::read(REPORTING_WASM).unwrap(),
    )
    .unwrap();
    Arc::new(rt)
}

fn ceo() -> FirmContext {
    FirmContext::new(
        Uuid::new_v4(),
        "tracy.brittcool@kanbrick.com",
        ClearanceLevel::L5,
    )
}

#[test]
fn a_completed_valuation_triggers_reporting() {
    let (_d, store) = seeded();
    let bus = EventBus::new();
    let rt = runtime(store, bus.clone());

    // When a valuation completes, regenerate the portfolio dashboard.
    let dashboards = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let sink = dashboards.clone();
    let rt_for_handler = rt.clone();
    bus.subscribe("valuation.completed", move |_event| {
        if let Ok(resp) = rt_for_handler.invoke(
            "reporting",
            &ceo(),
            &GuestRequest::new(json!({"report": "portfolio_dashboard"})),
        ) {
            sink.lock().unwrap().push(resp.payload);
        }
    });

    // Run a valuation; its emit fires the subscriber re-entrantly.
    let report = rt
        .invoke(
            "valuation",
            &ceo(),
            &GuestRequest::new(json!({"company_id": "JMTS"})),
        )
        .unwrap()
        .payload;

    // The valuation result is intact.
    assert_eq!(report["company_id"], json!("JMTS"));
    assert!(report["enterprise_value"].as_f64().unwrap() > 0.0);

    // The event was emitted and retained in the replayable log (the chain record).
    let history = bus.history();
    assert!(history
        .iter()
        .any(|e| e.kind == "valuation.completed" && e.payload["company_id"] == json!("JMTS")));

    // The reporting guest auto-ran and produced an updated dashboard.
    let dashes = dashboards.lock().unwrap();
    assert_eq!(dashes.len(), 1, "reporting regenerated exactly once");
    assert_eq!(dashes[0]["companies"].as_array().unwrap().len(), 9);
}

#[test]
fn a_reporting_failure_does_not_lose_the_valuation() {
    let (_d, store) = seeded();
    let bus = EventBus::new();
    let rt = runtime(store, bus.clone());

    // A subscriber that always fails (invokes an unregistered guest). The error is
    // swallowed in the handler; the valuation result must survive.
    let rt_for_handler = rt.clone();
    bus.subscribe("valuation.completed", move |_event| {
        let _ = rt_for_handler.invoke("no-such-guest", &ceo(), &GuestRequest::new(json!({})));
    });

    let report = rt
        .invoke(
            "valuation",
            &ceo(),
            &GuestRequest::new(json!({"company_id": "JMTS"})),
        )
        .unwrap()
        .payload;

    // Valuation completed and returned despite the failing subscriber.
    assert_eq!(report["company_id"], json!("JMTS"));
    assert!(report["enterprise_value"].as_f64().unwrap() > 0.0);
    assert_eq!(
        bus.history().len(),
        1,
        "the valuation event is still logged"
    );
}
