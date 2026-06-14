//! Issue #10 — versioned schema & seed migrations.

use kanbrick_store::{Migrator, Params, Store};

/// Path to the firm seed file, resolved relative to this crate.
fn seed_source() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../seed/kanbrick_seed_data.cypher"
    );
    std::fs::read_to_string(path).expect("seed file must be readable")
}

/// The runner applies the initial-schema migration then the seed migration in
/// order; a second run is a no-op; `MigrationLog` records each version.
#[test]
fn migrations_apply_in_order_and_are_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    let first = Migrator::firm(seed_source()).run(&store).unwrap();
    assert_eq!(
        first.applied,
        vec![1, 2],
        "both migrations apply on first run"
    );
    assert!(first.skipped.is_empty());

    let second = Migrator::firm(seed_source()).run(&store).unwrap();
    assert!(second.applied.is_empty(), "second run applies nothing");
    assert_eq!(second.skipped, vec![1, 2], "both versions already recorded");
}

/// `MigrationLog` records each applied version with a timestamp.
#[test]
fn migration_log_records_versions_with_timestamps() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    Migrator::firm(seed_source()).run(&store).unwrap();

    let logged = store
        .scalar_i64("MATCH (m:MigrationLog) RETURN count(m)", Params::new())
        .unwrap();
    assert_eq!(logged, Some(2), "one MigrationLog node per applied version");

    // Each log entry carries a non-empty applied_at timestamp.
    let stamped = store
        .scalar_i64(
            "MATCH (m:MigrationLog) WHERE m.applied_at <> '' RETURN count(m)",
            Params::new(),
        )
        .unwrap();
    assert_eq!(stamped, Some(2));
}

/// After migration, the person count matches the expected seed count and the
/// constraints from the initial migration are active.
#[test]
fn after_migration_seed_present_and_constraints_active() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    Migrator::firm(seed_source()).run(&store).unwrap();

    let people = store
        .scalar_i64("MATCH (p:Person) RETURN count(p)", Params::new())
        .unwrap();
    assert_eq!(people, Some(12), "all 12 seed persons loaded");

    let companies = store
        .scalar_i64("MATCH (c:Company) RETURN count(c)", Params::new())
        .unwrap();
    assert_eq!(companies, Some(9), "all 9 seed companies loaded");

    // The unique-email constraint from v001 is active: re-inserting an existing
    // seed email must be rejected.
    let dup = store.execute("CREATE (:Person {email: 'tracy.brittcool@kanbrick.com'})");
    assert!(
        dup.is_err(),
        "duplicate seed email must violate the active unique constraint"
    );
}
