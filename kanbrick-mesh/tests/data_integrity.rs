//! #50 — data-integrity validation: the compliance guest as a whole-system gate
//! over the seed data, plus direct graph-shape assertions. Runnable as a CI gate
//! (it is a normal `cargo test`).

use std::sync::Arc;

use kanbrick_core::abi::GuestRequest;
use kanbrick_core::{ClearanceLevel, FirmContext};
use kanbrick_mesh::MeshRuntime;
use kanbrick_store::{Migrator, Params, Store};
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

fn count(store: &Store, cypher: &str) -> i64 {
    store
        .scalar_i64(cypher, Params::new())
        .unwrap()
        .unwrap_or(0)
}

#[test]
fn compliance_gate_passes_on_seed_data() {
    let (_d, store) = seeded();
    let wasm = std::fs::read(COMPLIANCE_WASM).unwrap();
    let mut rt = MeshRuntime::new().unwrap().with_store(store);
    rt.register_module("compliance", "0.1.0", &wasm).unwrap();

    let ceo = FirmContext::new(
        Uuid::new_v4(),
        "tracy.brittcool@kanbrick.com",
        ClearanceLevel::L5,
    );
    let report = rt
        .invoke(
            "compliance",
            &ceo,
            &GuestRequest::new(json!({"check": "all"})),
        )
        .unwrap()
        .payload;

    // The whole-system integrity check passes with zero violations.
    assert_eq!(
        report["passed"],
        json!(true),
        "violations: {}",
        report["violations"]
    );
    assert!(report["violations"].as_array().unwrap().is_empty());
}

#[test]
fn graph_shape_is_intact() {
    let (_d, store) = seeded();

    // Population is exactly as seeded (single-label counts are reliable).
    assert_eq!(count(&store, "MATCH (p:Person) RETURN count(p)"), 12);
    assert_eq!(count(&store, "MATCH (c:Company) RETURN count(c)"), 9);
    assert_eq!(count(&store, "MATCH (s:Segment) RETURN count(s)"), 4);

    // Per ADR-0001, `count()` over a *path* is unreliable; project the nodes and
    // count in Rust instead (the pattern ClearanceScope/discovery use).
    let rows = |cypher: &str| -> usize {
        store
            .query::<serde_json::Value>(cypher, Params::new())
            .unwrap()
            .len()
    };

    // Every company belongs to a segment (no orphan companies).
    let companies_with_segment =
        rows("MATCH (c:Company)-[:BELONGS_TO_SEGMENT]->(s:Segment) RETURN c.company_id");
    assert_eq!(companies_with_segment, 9, "every company has a segment");

    // Every non-CEO person reaches the CEO through REPORTS_TO (no orphan people).
    let reach_ceo =
        rows("MATCH (p:Person)-[:REPORTS_TO*1..10]->(ceo:Person {role: \"CEO\"}) RETURN p.email");
    assert_eq!(reach_ceo, 11, "all non-CEO persons reach the CEO");
}

#[test]
fn guest_invocation_is_audited() {
    // #48 audit completeness: a guest's graph reads are recorded against the
    // caller through the GuardedStore the mesh routes every query_graph through.
    let (_d, store) = seeded();
    let wasm = std::fs::read(COMPLIANCE_WASM).unwrap();
    let mut rt = MeshRuntime::new().unwrap().with_store(store.clone());
    rt.register_module("compliance", "0.1.0", &wasm).unwrap();

    let ceo = FirmContext::new(
        Uuid::new_v4(),
        "tracy.brittcool@kanbrick.com",
        ClearanceLevel::L5,
    );
    rt.invoke(
        "compliance",
        &ceo,
        &GuestRequest::new(json!({"check": "all"})),
    )
    .unwrap();

    let audited = kanbrick_auth::AuditLog::new(&store)
        .count_for_user(ceo.user_id)
        .unwrap();
    assert!(
        audited > 0,
        "the guest's queries were audited under the caller"
    );
}
