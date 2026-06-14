//! Issue #11 — seed loader behavior (line-numbered errors, clearance levels).

use kanbrick_store::{seed, Migrator, Params, Store};

/// A malformed statement in a seed file produces a structured error that cites
/// the offending source line.
#[test]
fn malformed_seed_reports_line_number() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    // Line 1 is valid; line 2 is a comment; line 3 is garbage Cypher.
    let source = "CREATE (:Person {email: 'ok@kanbrick.com'});\n// a comment\nTHIS IS NOT CYPHER;";
    let err = seed::load_str(&store, source).expect_err("malformed seed must error");
    let msg = err.to_string();
    assert!(
        msg.contains("line 3"),
        "error should cite the offending line (3), got: {msg}"
    );
}

/// Loading the firm seed yields all 12 employees with their correct clearance
/// levels (L5=2, L4=4, L3=3, L2=2, L1=1).
#[test]
fn seed_loads_all_clearance_tiers() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../seed/kanbrick_seed_data.cypher"
    ))
    .unwrap();
    Migrator::firm(source).run(&store).unwrap();

    let by_level = |level: &str| -> i64 {
        store
            .scalar_i64(
                "MATCH (p:Person {clearance_level: $lvl}) RETURN count(p)",
                Params::new().with("lvl", level),
            )
            .unwrap()
            .unwrap_or(0)
    };

    assert_eq!(by_level("L5"), 2, "two L5 admins");
    assert_eq!(by_level("L4"), 4, "four L4 strategic leaders");
    assert_eq!(by_level("L3"), 3, "three L3 operational leads");
    assert_eq!(by_level("L2"), 2, "two L2 execution analysts");
    assert_eq!(by_level("L1"), 1, "one L1 support coordinator");
}
