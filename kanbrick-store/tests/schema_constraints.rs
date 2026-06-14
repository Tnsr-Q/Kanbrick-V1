//! Issue #8 — firm schema: node structs and uniqueness constraints.

use kanbrick_store::schema::{self, CompanyNode, PersonNode, SegmentNode};
use kanbrick_store::Store;

fn apply_schema(store: &Store) {
    for stmt in schema::schema_statements() {
        store.execute(stmt).expect("schema DDL must apply");
    }
}

/// Applying the schema constraints and indexes runs without error.
#[test]
fn schema_ddl_applies_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    apply_schema(&store);
}

/// Uniqueness is enforced on person email: inserting a duplicate email surfaces
/// a constraint-violation error.
#[test]
fn duplicate_person_email_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    apply_schema(&store);

    store
        .execute("CREATE (:Person {email: 'dup@kanbrick.com'})")
        .expect("first insert must succeed");

    let err = store
        .execute("CREATE (:Person {email: 'dup@kanbrick.com'})")
        .expect_err("duplicate email must violate the unique constraint");

    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("unique") || msg.contains("constraint") || msg.contains("violation"),
        "error should mention the constraint violation, got: {msg}"
    );
}

/// Uniqueness is enforced on company code (`company_id`).
#[test]
fn duplicate_company_code_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    apply_schema(&store);

    store
        .execute("CREATE (:Company {company_id: 'JMTS'})")
        .expect("first insert must succeed");

    store
        .execute("CREATE (:Company {company_id: 'JMTS'})")
        .expect_err("duplicate company_id must violate the unique constraint");
}

/// The node structs expose the seed-data fields and round-trip through JSON.
#[test]
fn node_structs_round_trip() {
    let person = PersonNode {
        full_name: "Tracy Britt Cool".into(),
        first_name: "Tracy".into(),
        last_name: "Britt Cool".into(),
        email: "tracy.brittcool@kanbrick.com".into(),
        title: "Chief Executive Officer".into(),
        role: "CEO".into(),
        clearance_level: kanbrick_core::ClearanceLevel::L5,
        clearance_label: "Admin".into(),
        department: "Executive".into(),
        status: kanbrick_core::Status::Active,
        segment: None,
        note: None,
    };
    let back: PersonNode = serde_json::from_str(&serde_json::to_string(&person).unwrap()).unwrap();
    assert_eq!(person, back);

    let company = CompanyNode {
        company_id: "JMTS".into(),
        name: "JM Test Systems".into(),
        legal_name: "JM Test Systems, Inc.".into(),
        segment: "Testing & Lab Services".into(),
        status: kanbrick_core::Status::Active,
        acquired_year: 2021,
        hq_state: "TX".into(),
        description: "Calibration, test equipment sales, and rental services".into(),
    };
    let back: CompanyNode =
        serde_json::from_str(&serde_json::to_string(&company).unwrap()).unwrap();
    assert_eq!(company, back);

    let segment = SegmentNode {
        name: "Testing & Lab Services".into(),
        code: "TLS".into(),
        description: "Testing, laboratory, and analytical services portfolio".into(),
    };
    let back: SegmentNode =
        serde_json::from_str(&serde_json::to_string(&segment).unwrap()).unwrap();
    assert_eq!(segment, back);
}
