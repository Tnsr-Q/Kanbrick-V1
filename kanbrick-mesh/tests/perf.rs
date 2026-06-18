//! #49 — performance benchmarks: SparrowDB query latency (p50/p95/p99), mesh
//! dispatch overhead, and WASM cold-start. Measured against the 12-person seed.
//!
//! CI runners are shared and noisy, so the assertions here are deliberately
//! **loose** (they catch gross regressions / hangs, not microsecond drift). Run
//! with `--nocapture` to print the measured numbers; representative figures and
//! the PRD targets (p99 query < 50 ms, dispatch < 5 ms, cold-start < 500 ms) are
//! recorded in `docs/benchmarks.md` for regression tracking.

use std::sync::Arc;
use std::time::Instant;

use kanbrick_core::abi::GuestRequest;
use kanbrick_core::{ClearanceLevel, FirmContext};
use kanbrick_mesh::MeshRuntime;
use kanbrick_store::{Migrator, Params, Store};
use serde_json::json;
use uuid::Uuid;

const COMPLIANCE_WASM: &str = env!("KANBRICK_COMPLIANCE_GUEST_WASM");
const ECHO_WASM: &str = env!("KANBRICK_ECHO_GUEST_WASM");

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

/// Percentile (nearest-rank) of a set of microsecond samples.
fn percentile(sorted_us: &[u128], pct: f64) -> u128 {
    if sorted_us.is_empty() {
        return 0;
    }
    let rank = ((pct / 100.0) * sorted_us.len() as f64).ceil() as usize;
    sorted_us[rank.clamp(1, sorted_us.len()) - 1]
}

#[test]
fn sparrowdb_query_latency() {
    let (_d, store) = seeded();
    const N: usize = 300;
    let mut samples = Vec::with_capacity(N);
    for _ in 0..N {
        let start = Instant::now();
        let _ = store
            .query::<serde_json::Value>(
                "MATCH (c:Company) RETURN c.company_id, c.name, c.segment",
                Params::new(),
            )
            .unwrap();
        samples.push(start.elapsed().as_micros());
    }
    samples.sort_unstable();
    let p50 = percentile(&samples, 50.0);
    let p95 = percentile(&samples, 95.0);
    let p99 = percentile(&samples, 99.0);
    println!("query latency  p50={p50}us  p95={p95}us  p99={p99}us  (target p99 < 50000us)");

    // Loose gross-regression guard (CI noise-tolerant).
    assert!(
        p99 < 1_000_000,
        "p99 query latency {p99}us is implausibly high"
    );
}

#[test]
fn mesh_dispatch_and_cold_start() {
    let (_d, store) = seeded();
    let wasm = std::fs::read(COMPLIANCE_WASM).unwrap();

    // Cold start: build runtime + register (compile) the guest + first invoke.
    let cold = Instant::now();
    let mut rt = MeshRuntime::new().unwrap().with_store(store);
    rt.register_module("compliance", "0.1.0", &wasm).unwrap();
    let ceo = FirmContext::new(
        Uuid::new_v4(),
        "tracy.brittcool@kanbrick.com",
        ClearanceLevel::L5,
    );
    rt.invoke(
        "compliance",
        &ceo,
        &GuestRequest::new(json!({"check": "org_chart"})),
    )
    .unwrap();
    let cold_ms = cold.elapsed().as_millis();
    println!(
        "module compile + first invoke (compliance) = {cold_ms}ms  \
         (Cranelift compile dominates; serialize modules to cut this — see docs/benchmarks.md)"
    );

    // Pure mesh dispatch overhead: the echo guest does NO graph queries, so this
    // isolates instantiate + call + teardown from guest work.
    let echo = std::fs::read(ECHO_WASM).unwrap();
    rt.register_module("echo", "0.0.0", &echo).unwrap();
    const N: usize = 50;
    let mut dispatch = Vec::with_capacity(N);
    for _ in 0..N {
        let start = Instant::now();
        rt.dispatch("echo", b"ping").unwrap();
        dispatch.push(start.elapsed().as_micros());
    }
    dispatch.sort_unstable();
    println!(
        "mesh dispatch overhead (echo) p50={}us  p99={}us  (target < 5000us)",
        percentile(&dispatch, 50.0),
        percentile(&dispatch, 99.0)
    );
    assert!(
        percentile(&dispatch, 99.0) < 500_000,
        "dispatch overhead implausibly high"
    );

    // Full guest execution (compliance = ~16 re-entrant graph queries) for context.
    let exec = Instant::now();
    rt.invoke(
        "compliance",
        &ceo,
        &GuestRequest::new(json!({"check": "all"})),
    )
    .unwrap();
    println!(
        "full compliance execution (16 graph queries) = {}us",
        exec.elapsed().as_micros()
    );
}
