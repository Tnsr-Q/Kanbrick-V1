//! Issue #9 — parameterized, injection-safe, typed queries.

use kanbrick_store::schema::PersonNode;
use kanbrick_store::{Params, Store};

/// Create a Person via a parameterized statement, read it back through a
/// parameterized lookup, and deserialize into `PersonNode` with field equality.
#[test]
fn create_query_deserialize_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    // SparrowDB's parameterized write path is MERGE with inline properties
    // (standalone CREATE with $params is unsupported). Every property is bound
    // through a $param, never interpolated into the query text.
    let create = "MERGE (p:Person { \
        full_name: $full_name, first_name: $first_name, last_name: $last_name, \
        email: $email, title: $title, role: $role, \
        clearance_level: $clearance_level, clearance_label: $clearance_label, \
        department: $department, status: $status })";
    let params = Params::new()
        .with("full_name", "Samantha Jordan")
        .with("first_name", "Samantha")
        .with("last_name", "Jordan")
        .with("email", "samantha.jordan@kanbrick.com")
        .with("title", "Senior Investment Analyst")
        .with("role", "Senior Analyst")
        .with("clearance_level", "L2")
        .with("clearance_label", "Execution")
        .with("department", "Business Development")
        .with("status", "active");
    store.execute_with(create, params).unwrap();

    let lookup = "MATCH (p:Person {email: $email}) RETURN \
        p.full_name AS full_name, p.first_name AS first_name, p.last_name AS last_name, \
        p.email AS email, p.title AS title, p.role AS role, \
        p.clearance_level AS clearance_level, p.clearance_label AS clearance_label, \
        p.department AS department, p.status AS status";
    let found: Option<PersonNode> = store
        .query_one(
            lookup,
            Params::new().with("email", "samantha.jordan@kanbrick.com"),
        )
        .unwrap();

    let person = found.expect("person must be found");
    assert_eq!(person.full_name, "Samantha Jordan");
    assert_eq!(person.email, "samantha.jordan@kanbrick.com");
    assert_eq!(person.clearance_level, kanbrick_core::ClearanceLevel::L2);
    assert_eq!(person.status, kanbrick_core::Status::Active);
}

/// A malicious parameter value cannot alter query structure: it is bound as an
/// opaque string, so it matches no row and executes no injected statement.
#[test]
fn parameter_injection_is_neutralized() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    store
        .execute("CREATE (:Person {email: 'real@kanbrick.com', full_name: 'Real Person'})")
        .unwrap();

    // Classic injection payload as a parameter value.
    let payload = "real@kanbrick.com' OR '1'='1";
    let rows: Vec<PersonNode> = store
        .query(
            "MATCH (p:Person {email: $email}) RETURN \
             p.full_name AS full_name, p.first_name AS first_name, p.last_name AS last_name, \
             p.email AS email, p.title AS title, p.role AS role, \
             p.clearance_level AS clearance_level, p.clearance_label AS clearance_label, \
             p.department AS department, p.status AS status",
            Params::new().with("email", payload),
        )
        .unwrap_or_default();
    assert!(
        rows.is_empty(),
        "injection payload must not match the real row via OR '1'='1'"
    );

    // The original record is still intact and reachable with the real value.
    let count = store
        .scalar_i64(
            "MATCH (p:Person {email: $email}) RETURN count(p)",
            Params::new().with("email", "real@kanbrick.com"),
        )
        .unwrap();
    assert_eq!(count, Some(1));

    // And the injection attempt neither panicked nor dropped/altered the graph.
    let total = store
        .scalar_i64("MATCH (p:Person) RETURN count(p)", Params::new())
        .unwrap();
    assert_eq!(
        total,
        Some(1),
        "graph must be unchanged after injection attempt"
    );
}
