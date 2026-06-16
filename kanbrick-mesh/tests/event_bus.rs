//! #27 end-to-end: an event drives a guest. When a `valuation.completed` event
//! is published, a subscription reacts by invoking the reporting guest — the
//! `ValuationComplete ⇒ reporting guest` flow from the acceptance criteria.

use std::sync::{Arc, Mutex};

use kanbrick_core::abi::{Event, GuestRequest, GuestResponse};
use kanbrick_core::{ClearanceLevel, FirmContext};
use kanbrick_mesh::{EventBus, MeshRuntime};
use uuid::Uuid;

/// "Reporting" guest: turns its request payload into a response (echo stands in
/// for report generation).
const REPORTING_WAT: &str = r#"
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

#[test]
fn a_valuation_event_triggers_the_reporting_guest() {
    let mut rt = MeshRuntime::new().unwrap();
    rt.register_module("reporting", "0.1.0", REPORTING_WAT.as_bytes())
        .unwrap();
    let runtime = Arc::new(rt);
    let ctx = FirmContext::new(Uuid::new_v4(), "analyst@kanbrick.com", ClearanceLevel::L3);

    let bus = EventBus::new();
    let reports: Arc<Mutex<Vec<GuestResponse>>> = Arc::new(Mutex::new(Vec::new()));

    // Subscribe: on a completed valuation, run the reporting guest on the payload.
    let handler_rt = runtime.clone();
    let handler_ctx = ctx.clone();
    let sink = reports.clone();
    bus.subscribe("valuation.completed", move |event: &Event| {
        let request = GuestRequest::new(event.payload.clone());
        let response = handler_rt
            .invoke("reporting", &handler_ctx, &request)
            .expect("reporting guest runs");
        sink.lock().unwrap().push(response);
    });

    // The valuation guest completes (here the host emits on its behalf).
    let notified = bus.emit(Event::with_payload(
        "valuation.completed",
        serde_json::json!({"company_id": "ACME", "npv": 9_900_000}),
    ));
    assert_eq!(notified, 1);

    // The reporting guest produced a report from the valuation payload.
    let reports = reports.lock().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(
        reports[0].payload,
        serde_json::json!({"company_id": "ACME", "npv": 9_900_000})
    );

    // The event is retained in the replayable log either way.
    assert_eq!(bus.history().len(), 1);
}
