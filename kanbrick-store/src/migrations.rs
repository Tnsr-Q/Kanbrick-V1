//! Versioned schema & seed migrations (issue #10).
//!
//! A [`Migrator`] applies an ordered list of [`Migration`]s. Each applied
//! version is recorded as a `(:MigrationLog {version, name, applied_at})` node,
//! so the runner is idempotent: re-running skips versions already present.
//!
//! The default migration set is:
//!
//! * **v001 — initial schema**: the constraints and indexes from
//!   [`crate::schema::schema_statements`].
//! * **v002 — seed data**: load a Cypher seed file (e.g. the firm seed).

use std::collections::HashSet;

use kanbrick_core::Result;

use crate::store::Store;
use crate::{schema, seed};

/// The action a [`Migration`] performs against the store.
type ApplyFn = Box<dyn Fn(&Store) -> Result<()> + Send + Sync>;

/// A single, versioned migration step.
pub struct Migration {
    /// Monotonic version number; migrations apply in ascending order.
    pub version: u32,
    /// Human-readable name, recorded in the `MigrationLog`.
    pub name: String,
    apply: ApplyFn,
}

impl Migration {
    /// Construct a migration from a version, name, and an apply closure.
    pub fn new(
        version: u32,
        name: impl Into<String>,
        apply: impl Fn(&Store) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        Migration {
            version,
            name: name.into(),
            apply: Box::new(apply),
        }
    }
}

impl std::fmt::Debug for Migration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Migration")
            .field("version", &self.version)
            .field("name", &self.name)
            .finish()
    }
}

/// Summary of a [`Migrator::run`] invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationOutcome {
    /// Versions applied during this run, in order.
    pub applied: Vec<u32>,
    /// Versions skipped because they were already recorded.
    pub skipped: Vec<u32>,
}

/// Applies an ordered set of migrations against a [`Store`].
#[derive(Debug)]
pub struct Migrator {
    migrations: Vec<Migration>,
}

impl Migrator {
    /// Build a migrator from migrations (sorted ascending by version).
    pub fn new(mut migrations: Vec<Migration>) -> Self {
        migrations.sort_by_key(|m| m.version);
        Migrator { migrations }
    }

    /// The default firm migrations: initial schema, then seed data loaded from
    /// `seed_source` (the contents of a `.cypher` seed file).
    pub fn firm(seed_source: String) -> Self {
        Migrator::new(vec![
            Migration::new(1, "initial_schema", |store| {
                for stmt in schema::schema_statements() {
                    store.execute(stmt)?;
                }
                Ok(())
            }),
            Migration::new(2, "seed_data", move |store| {
                seed::load_str(store, &seed_source).map(|_| ())
            }),
        ])
    }

    /// Apply all pending migrations in order, recording each in the
    /// `MigrationLog`. Already-recorded versions are skipped.
    pub fn run(&self, store: &Store) -> Result<MigrationOutcome> {
        let already = applied_versions(store)?;
        let mut outcome = MigrationOutcome {
            applied: Vec::new(),
            skipped: Vec::new(),
        };

        for migration in &self.migrations {
            if already.contains(&migration.version) {
                outcome.skipped.push(migration.version);
                continue;
            }
            tracing::info!(
                target: "kanbrick_store::migrations",
                version = migration.version,
                name = %migration.name,
                "applying migration"
            );
            (migration.apply)(store)?;
            record_applied(store, migration.version, &migration.name)?;
            outcome.applied.push(migration.version);
        }

        Ok(outcome)
    }
}

/// Read the set of already-applied migration versions from the `MigrationLog`.
fn applied_versions(store: &Store) -> Result<HashSet<u32>> {
    // Querying an as-yet-unused label returns no rows on a fresh database.
    let result = store.execute("MATCH (m:MigrationLog) RETURN m.version AS version")?;
    let mut versions = HashSet::new();
    for row in &result.rows {
        if let Some(cell) = row.first() {
            if let serde_json::Value::Number(n) = crate::value::value_to_json(cell) {
                if let Some(v) = n.as_i64() {
                    versions.insert(v as u32);
                }
            }
        }
    }
    Ok(versions)
}

/// Record a successfully applied migration as a `MigrationLog` node.
fn record_applied(store: &Store, version: u32, name: &str) -> Result<()> {
    let params = crate::value::Params::new()
        .with("version", version as i64)
        .with("name", name)
        .with("applied_at", chrono::Utc::now().to_rfc3339());
    // SparrowDB's parameterized write path is MERGE with inline properties
    // (standalone parameterized CREATE is unsupported). Versions are unique, so
    // MERGE records exactly one log node per migration.
    store.execute_with(
        "MERGE (m:MigrationLog {version: $version, name: $name, applied_at: $applied_at})",
        params,
    )?;
    Ok(())
}
