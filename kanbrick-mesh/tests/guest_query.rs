//! #24 end-to-end: a WASM guest issues `query_graph` through the re-entrant host
//! import, and the host routes it through `GuardedStore` so the rows the guest
//! receives are clearance-filtered for its caller.

use std::sync::Arc;

use kanbrick_core::abi::{GraphQuery, GraphRows};
use kanbrick_core::{ClearanceLevel, FirmContext};
use kanbrick_mesh::MeshRuntime;
use kanbrick_store::{Migrator, Store};
use uuid::Uuid;

/// A hermetic "query proxy" guest: it forwards its input (a `GraphQuery` JSON the
/// host wrote into guest memory) to `kbk_query_graph` and returns the resulting
/// rows verbatim. This exercises the full re-entrant path — the host reads the
/// query, runs it through `GuardedStore`, calls back into the guest's `kbk_alloc`
/// to allocate space, and writes the rows back.
const QUERY_PROXY_WAT: &str = r#"
    (module
      (import "kanbrick" "kbk_query_graph" (func $query (param i32 i32) (result i64)))
      (memory (export "memory") 1)
      (global $next (mut i32) (i32.const 1024))
      (func (export "kbk_alloc") (param $len i32) (result i32)
        (local $p i32)
        global.get $next
        local.set $p
        global.get $next
        local.get $len
        i32.add
        global.set $next
        local.get $p)
      (func (export "kbk_run") (param $ptr i32) (param $len i32) (result i64)
        local.get $ptr
        local.get $len
        call $query))
"#;

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

fn all_companies_query() -> Vec<u8> {
    serde_json::to_vec(&GraphQuery::new(
        "MATCH (c:Company) RETURN c.company_id, c.name",
    ))
    .unwrap()
}

#[test]
fn guest_query_is_clearance_filtered_end_to_end() {
    let (_d, store) = seeded();
    let mut rt = MeshRuntime::new().unwrap().with_store(store);
    rt.register_module("q", "0.1.0", QUERY_PROXY_WAT.as_bytes())
        .unwrap();

    let query = all_companies_query();

    // The same guest, issuing the same query, sees different rows depending on
    // the host-authoritative caller: an L3 lead sees only their 5 segment
    // companies; the L5 CEO sees all 9. The guest cannot influence this.
    let l3 = FirmContext::new(
        Uuid::new_v4(),
        "tyler.begemann@kanbrick.com",
        ClearanceLevel::L3,
    );
    let out = rt.run_with_context("q", &l3, &query).unwrap();
    let rows: GraphRows = serde_json::from_slice(&out).unwrap();
    assert_eq!(rows.len(), 5);

    let l5 = FirmContext::new(
        Uuid::new_v4(),
        "tracy.brittcool@kanbrick.com",
        ClearanceLevel::L5,
    );
    let out = rt.run_with_context("q", &l5, &query).unwrap();
    let rows: GraphRows = serde_json::from_slice(&out).unwrap();
    assert_eq!(rows.len(), 9);
}

#[test]
fn guest_query_against_an_unfilterable_projection_is_denied() {
    let (_d, store) = seeded();
    let mut rt = MeshRuntime::new().unwrap().with_store(store);
    rt.register_module("q", "0.1.0", QUERY_PROXY_WAT.as_bytes())
        .unwrap();

    // An L3 caller projecting neither email nor company_id is denied (fail-closed)
    // — the denial surfaces to the guest as a trapped query call.
    let query = serde_json::to_vec(&GraphQuery::new("MATCH (c:Company) RETURN c.name")).unwrap();
    let l3 = FirmContext::new(
        Uuid::new_v4(),
        "tyler.begemann@kanbrick.com",
        ClearanceLevel::L3,
    );
    assert!(rt.run_with_context("q", &l3, &query).is_err());
}

#[test]
fn guest_query_without_a_bound_store_traps() {
    // No store bound: the query import exists but has no graph to serve.
    let mut rt = MeshRuntime::new().unwrap();
    rt.register_module("q", "0.1.0", QUERY_PROXY_WAT.as_bytes())
        .unwrap();
    let ctx = FirmContext::new(Uuid::new_v4(), "x@kanbrick.com", ClearanceLevel::L5);
    assert!(rt
        .run_with_context("q", &ctx, &all_companies_query())
        .is_err());
}
