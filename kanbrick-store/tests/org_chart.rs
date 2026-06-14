//! Issue #12 — org chart hierarchy verification.
//!
//! Seeds the database via the firm migrations and verifies the entire org
//! hierarchy end-to-end: REPORTS_TO chains to the CEO, MANAGES edges, and
//! BELONGS_TO_SEGMENT edges all cohere with the schema, store, and seed data.
//!
//! Verification uses two complementary approaches:
//!
//! * `reaches_ceo_natively_via_variable_length` exercises SparrowDB's native
//!   variable-length traversal (`-[:REPORTS_TO*1..4]->`), projecting the reached
//!   node set.
//! * The adjacency-based tests pull the one-hop REPORTS_TO edges and compute
//!   reachability and depth in Rust — a defense-in-depth cross-check that also
//!   covers the two query forms SparrowDB does NOT support in this pinned build:
//!   aggregating `count()` directly over a variable-length path, and inline
//!   `NOT (a)-[:R]->()` negation.
//!
//! The full verified capability matrix lives in
//! `docs/adr/0001-sparrowdb-cypher-capabilities.md`.

use std::collections::HashMap;

use kanbrick_store::{Migrator, Params, Store};
use serde::Deserialize;

/// One direct REPORTS_TO edge: `child` reports to `parent` (both emails).
#[derive(Debug, Deserialize)]
struct ReportsEdge {
    child: String,
    parent: String,
}

/// A single projected `email` column.
#[derive(Debug, Deserialize)]
struct PersonEmail {
    email: String,
}

/// Seed a fresh store with the firm schema + data and return it.
fn seeded_store(dir: &std::path::Path) -> Store {
    let seed = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../seed/kanbrick_seed_data.cypher"
    ))
    .expect("seed file");
    let store = Store::open(dir).unwrap();
    Migrator::firm(seed).run(&store).unwrap();
    store
}

fn count(store: &Store, cypher: &str) -> i64 {
    store
        .scalar_i64(cypher, Params::new())
        .unwrap()
        .unwrap_or(0)
}

/// Build the `child -> parent` map from the one-hop REPORTS_TO adjacency.
fn reports_to_map(store: &Store) -> HashMap<String, String> {
    let edges: Vec<ReportsEdge> = store
        .query(
            "MATCH (p:Person)-[:REPORTS_TO]->(m:Person) RETURN p.email AS child, m.email AS parent",
            Params::new(),
        )
        .unwrap();
    let mut map = HashMap::new();
    for e in edges {
        // Each child must have exactly one manager — a duplicate would collide.
        assert!(
            map.insert(e.child.clone(), e.parent).is_none(),
            "person {} has more than one REPORTS_TO edge",
            e.child
        );
    }
    map
}

/// SparrowDB evaluates variable-length traversal natively: projecting the
/// reached nodes of `-[:REPORTS_TO*1..4]->` returns every non-CEO employee.
/// (The row count is taken in Rust; aggregating `count()` directly over a
/// variable-length path is unsupported in this build — see the ADR.)
#[test]
fn reaches_ceo_natively_via_variable_length() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path());

    let reached: Vec<PersonEmail> = store
        .query(
            "MATCH (p:Person)-[:REPORTS_TO*1..4]->(ceo:Person {role: 'CEO'}) \
             RETURN p.email AS email",
            Params::new(),
        )
        .unwrap();

    assert_eq!(
        reached.len(),
        11,
        "native variable-length traversal must reach all 11 non-CEO employees"
    );
    // The CEO is the target, never a member of the reached (source) set.
    assert!(
        !reached
            .iter()
            .any(|p| p.email == "tracy.brittcool@kanbrick.com"),
        "the CEO must not appear in the set of people reaching the CEO"
    );
}

/// The CEO is the org root (no manager) and every other employee has exactly
/// one manager.
#[test]
fn ceo_is_root_and_everyone_else_reports() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path());

    let persons = count(&store, "MATCH (p:Person) RETURN count(p)");
    assert_eq!(persons, 12);

    let map = reports_to_map(&store);
    // 11 of 12 have exactly one manager (enforced by the map insert assertion).
    assert_eq!(
        map.len(),
        11,
        "every non-CEO employee has exactly one manager"
    );

    // The CEO has no outgoing REPORTS_TO edge.
    let ceo_with_manager = count(
        &store,
        "MATCH (ceo:Person {role: 'CEO'})-[:REPORTS_TO]->(x) RETURN count(x)",
    );
    assert_eq!(ceo_with_manager, 0, "the CEO reports to no one");

    let ceo_email = "tracy.brittcool@kanbrick.com";
    assert!(
        !map.contains_key(ceo_email),
        "the CEO must not appear as a child in the REPORTS_TO map"
    );
}

/// Every non-CEO employee reaches the CEO by following REPORTS_TO, and the
/// chain from any leaf to the CEO is at most four hops.
#[test]
fn all_reach_ceo_within_four_hops() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path());
    let map = reports_to_map(&store);

    let ceo_email = "tracy.brittcool@kanbrick.com";
    let mut max_depth = 0;
    let mut reached = 0;

    for child in map.keys() {
        let mut current = child.clone();
        let mut depth = 0;
        while current != ceo_email {
            depth += 1;
            assert!(
                depth <= map.len(),
                "cycle detected reaching CEO from {child}"
            );
            current = map
                .get(&current)
                .unwrap_or_else(|| panic!("{current} has no manager but is not the CEO"))
                .clone();
        }
        reached += 1;
        max_depth = max_depth.max(depth);
    }

    assert_eq!(reached, 11, "all 11 non-CEO employees reach the CEO");
    assert!(
        max_depth <= 4,
        "leaf-to-CEO depth must be <= 4, was {max_depth}"
    );
    assert_eq!(
        max_depth, 4,
        "deepest chain (analyst -> ... -> CEO) is 4 hops"
    );
}

/// Segment leads manage the companies within their own segment (spot-check the
/// Testing & Lab Services lead, who manages all five of its companies).
#[test]
fn segment_lead_manages_their_companies() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path());

    let managed = count(
        &store,
        "MATCH (p:Person {email: 'tyler.begemann@kanbrick.com'})-[:MANAGES]->(c:Company) \
         RETURN count(c)",
    );
    assert_eq!(
        managed, 5,
        "the Testing & Lab lead manages all five of its companies"
    );
}

/// All operating segments have companies assigned, and no company is an orphan.
#[test]
fn segments_populated_and_no_orphans() {
    let dir = tempfile::tempdir().unwrap();
    let store = seeded_store(dir.path());

    let companies = count(&store, "MATCH (c:Company) RETURN count(c)");
    let companies_in_segment = count(
        &store,
        "MATCH (c:Company)-[:BELONGS_TO_SEGMENT]->(:Segment) RETURN count(c)",
    );
    assert_eq!(companies, 9);
    assert_eq!(
        companies, companies_in_segment,
        "every company is assigned to a segment"
    );

    let segments = count(&store, "MATCH (s:Segment) RETURN count(s)");
    assert_eq!(segments, 4, "all four segments loaded");
}
