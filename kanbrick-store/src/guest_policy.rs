//! Persisted guest policy (#64, Track C).
//!
//! A `(:GuestPolicy)` node binds a guest **name** to the version, minimum
//! clearance, and content-addressed asset URI that should serve it, plus a
//! `source` discriminating boot-embedded guests from registry-activated ones.
//! SparrowDB is the source of truth (mirroring the project's "business state
//! lives in the graph" convention); the asset *bytes* live on the content-
//! addressed volume (`kanbrick_mesh::assets`).
//!
//! Writes follow the ADR-0001 dialect: a parameterized `MERGE` on the key
//! followed by `MATCH … SET` for the mutable fields, so re-activating a guest
//! **updates in place** rather than creating a duplicate node. `min_clearance`
//! is stored as its `Display` form (`"L3"`), which is exactly the `serde` wire
//! form of [`ClearanceLevel`], so it round-trips on read.

use kanbrick_core::{ClearanceLevel, Result};
use serde::{Deserialize, Serialize};

use crate::store::Store;
use crate::value::Params;

/// A boot-embedded guest baked into the binary.
pub const SOURCE_EMBEDDED: &str = "embedded";
/// A guest activated at runtime from the asset registry.
pub const SOURCE_REGISTRY: &str = "registry";

/// A guest's persisted policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestPolicy {
    /// Unique guest name (the node key).
    pub guest_name: String,
    /// The version currently bound to the guest.
    pub version: String,
    /// Minimum clearance required to invoke the guest.
    pub min_clearance: ClearanceLevel,
    /// Content-addressed asset URI (`tachyon://sha256:…`); empty for embedded
    /// guests, which are served from the binary.
    pub asset_uri: String,
    /// `"embedded"` or `"registry"` (see the `SOURCE_*` constants).
    pub source: String,
    /// RFC 3339 timestamp of when this policy was last written.
    pub created_at: String,
}

impl GuestPolicy {
    /// Build a policy, stamping `created_at` with the current time.
    pub fn new(
        guest_name: impl Into<String>,
        version: impl Into<String>,
        min_clearance: ClearanceLevel,
        asset_uri: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        GuestPolicy {
            guest_name: guest_name.into(),
            version: version.into(),
            min_clearance,
            asset_uri: asset_uri.into(),
            source: source.into(),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Whether this guest is served from the runtime asset registry (as opposed
    /// to being embedded in the binary).
    pub fn is_registry(&self) -> bool {
        self.source == SOURCE_REGISTRY
    }
}

/// Columns projected by the read queries, in order. Un-aliased bare-node
/// projection per ADR-0001 (the row mapper strips the `g.` prefix); aliasing to a
/// different name would yield nulls on this build.
const PROJECTION: &str = "RETURN g.guest_name, g.version, g.min_clearance, \
     g.asset_uri, g.source, g.created_at";

/// Insert or update a guest policy, keyed by `guest_name`.
pub fn write_guest_policy(store: &Store, policy: &GuestPolicy) -> Result<()> {
    // MERGE the node on its key alone, then SET the mutable fields, so a
    // re-activation with a new version/clearance updates the existing node
    // instead of creating a second one (ADR-0001: standalone `CREATE` is
    // unsupported; `MERGE` + `MATCH … SET` is the upsert path).
    store.execute_with(
        "MERGE (g:GuestPolicy {guest_name: $guest_name})",
        Params::new().with("guest_name", policy.guest_name.as_str()),
    )?;
    store.execute_with(
        "MATCH (g:GuestPolicy {guest_name: $guest_name}) \
         SET g.version = $version, g.min_clearance = $min_clearance, \
             g.asset_uri = $asset_uri, g.source = $source, g.created_at = $created_at",
        Params::new()
            .with("guest_name", policy.guest_name.as_str())
            .with("version", policy.version.as_str())
            .with("min_clearance", policy.min_clearance.to_string())
            .with("asset_uri", policy.asset_uri.as_str())
            .with("source", policy.source.as_str())
            .with("created_at", policy.created_at.as_str()),
    )?;
    Ok(())
}

/// Read a guest's policy by name, or `None` if it has none.
pub fn read_guest_policy(store: &Store, guest_name: &str) -> Result<Option<GuestPolicy>> {
    store.query_one::<GuestPolicy>(
        &format!("MATCH (g:GuestPolicy {{guest_name: $guest_name}}) {PROJECTION}"),
        Params::new().with("guest_name", guest_name),
    )
}

/// Read every guest policy.
pub fn list_guest_policies(store: &Store) -> Result<Vec<GuestPolicy>> {
    store.query::<GuestPolicy>(
        &format!("MATCH (g:GuestPolicy) {PROJECTION}"),
        Params::new(),
    )
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
    fn write_then_read_round_trips() {
        let (_d, store) = store();
        let policy = GuestPolicy::new(
            "valuation",
            "0.1.0",
            ClearanceLevel::L3,
            "",
            SOURCE_EMBEDDED,
        );
        write_guest_policy(&store, &policy).unwrap();

        let read = read_guest_policy(&store, "valuation").unwrap().unwrap();
        assert_eq!(read.guest_name, "valuation");
        assert_eq!(read.version, "0.1.0");
        assert_eq!(read.min_clearance, ClearanceLevel::L3);
        assert_eq!(read.source, SOURCE_EMBEDDED);
        assert!(!read.is_registry());
    }

    #[test]
    fn missing_policy_is_none() {
        let (_d, store) = store();
        assert!(read_guest_policy(&store, "ghost").unwrap().is_none());
    }

    #[test]
    fn re_activation_updates_in_place() {
        let (_d, store) = store();
        write_guest_policy(
            &store,
            &GuestPolicy::new(
                "valuation",
                "0.1.0",
                ClearanceLevel::L3,
                "",
                SOURCE_EMBEDDED,
            ),
        )
        .unwrap();
        write_guest_policy(
            &store,
            &GuestPolicy::new(
                "valuation",
                "0.2.0",
                ClearanceLevel::L4,
                "tachyon://sha256:abc",
                SOURCE_REGISTRY,
            ),
        )
        .unwrap();

        let read = read_guest_policy(&store, "valuation").unwrap().unwrap();
        assert_eq!(read.version, "0.2.0", "version updated");
        assert_eq!(read.min_clearance, ClearanceLevel::L4, "clearance updated");
        assert!(read.is_registry(), "source updated");
        assert_eq!(read.asset_uri, "tachyon://sha256:abc");

        // Exactly one node — no duplicate created by the second write.
        let all = list_guest_policies(&store).unwrap();
        assert_eq!(
            all.iter().filter(|p| p.guest_name == "valuation").count(),
            1
        );
    }

    #[test]
    fn list_returns_all_policies() {
        let (_d, store) = store();
        write_guest_policy(
            &store,
            &GuestPolicy::new("valuation", "1", ClearanceLevel::L3, "", SOURCE_EMBEDDED),
        )
        .unwrap();
        write_guest_policy(
            &store,
            &GuestPolicy::new("reporting", "1", ClearanceLevel::L1, "", SOURCE_EMBEDDED),
        )
        .unwrap();

        let mut names: Vec<String> = list_guest_policies(&store)
            .unwrap()
            .into_iter()
            .map(|p| p.guest_name)
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["reporting".to_string(), "valuation".to_string()]
        );
    }
}
