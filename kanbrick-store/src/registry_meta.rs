//! Persisted registry generation counter (#69, Track E).
//!
//! A single `(:RegistryMeta {id: "singleton"})` node holds a monotonic
//! `generation` that is bumped on every guest **activation** (`activate_guest`).
//! Executors (#70) read it on boot and poll it to detect when the
//! registry-activated guest set has changed, so they can reconcile (re-pull
//! assets + hot-reload). It is **persisted** rather than in-memory so it survives
//! a control-plane restart and never moves backwards (which would make executors
//! miss a change).
//!
//! Writes follow the ADR-0001 dialect: a parameterized `MERGE` on the singleton
//! key, then `MATCH … SET` for the counter — the same upsert shape as
//! [`guest_policy`](crate::guest_policy).

use kanbrick_core::Result;

use crate::store::Store;
use crate::value::Params;

/// The fixed key value of the singleton generation node. (`slot` is used as the
/// key property rather than `id`, which is a Cypher built-in.)
const SLOT: &str = "registry-generation";

/// Read the current registry generation, or `0` if no activation has happened
/// yet (the node is absent on a fresh store).
pub fn read_registry_generation(store: &Store) -> Result<u64> {
    let generation = store.scalar_i64(
        "MATCH (m:RegistryMeta {slot: $slot}) RETURN m.generation",
        Params::new().with("slot", SLOT),
    )?;
    // Stored as a non-negative i64; clamp defensively before widening to u64.
    Ok(generation.unwrap_or(0).max(0) as u64)
}

/// Increment the registry generation and return the new value. Called after a
/// guest activation persists its policy, so executors observe the change.
pub fn bump_registry_generation(store: &Store) -> Result<u64> {
    let next = read_registry_generation(store)?.saturating_add(1);
    // MERGE on the key alone, then SET the counter — standalone parameterized
    // `CREATE` is unsupported on this SparrowDB build (ADR-0001).
    store.execute_with(
        "MERGE (m:RegistryMeta {slot: $slot})",
        Params::new().with("slot", SLOT),
    )?;
    store.execute_with(
        "MATCH (m:RegistryMeta {slot: $slot}) SET m.generation = $generation",
        Params::new()
            .with("slot", SLOT)
            .with("generation", next as i64),
    )?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn fresh_store_reads_zero() {
        let (_d, store) = store();
        assert_eq!(read_registry_generation(&store).unwrap(), 0);
    }

    #[test]
    fn bump_increments_monotonically_in_place() {
        let (_d, store) = store();
        assert_eq!(bump_registry_generation(&store).unwrap(), 1);
        assert_eq!(bump_registry_generation(&store).unwrap(), 2);
        assert_eq!(bump_registry_generation(&store).unwrap(), 3);
        assert_eq!(read_registry_generation(&store).unwrap(), 3);

        // Exactly one singleton node — no duplicates from repeated bumps.
        let count = store
            .scalar_i64("MATCH (m:RegistryMeta) RETURN count(m)", Params::new())
            .unwrap()
            .unwrap_or(0);
        assert_eq!(count, 1);
    }

    #[test]
    fn generation_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = Store::open(dir.path()).unwrap();
            bump_registry_generation(&store).unwrap();
            bump_registry_generation(&store).unwrap();
            store.checkpoint().unwrap();
        }
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(
            read_registry_generation(&store).unwrap(),
            2,
            "generation persists across a restart"
        );
    }
}
