//! # kanbrick-discovery
//!
//! Graph analysis over the firm graph — Layer 4 ("Map").
//!
//! Phase 4 wires the firm graph (loaded from SparrowDB) into the graphify-rs
//! library sub-crates and exposes firm-typed discovery operations: org-chart
//! analytics (reporting paths, span of control, neighborhoods, common managers),
//! portfolio analytics (company stakeholders, segment overviews, cross-segment
//! links), structural influence ranking, and a clearance-aware result surface.
//!
//! ## Shape (see ADR-0003)
//!
//! * graphify-rs 0.8 is a CLI *binary*; the reusable library is its sub-crates
//!   (`graphify-core`'s `KnowledgeGraph`, `graphify-analyze`, …). We build the
//!   firm graph in memory once and analyze it there.
//! * The graphify graph is **undirected**, so the directed org algorithms are
//!   computed by us from the edge payloads; graphify supplies the container and
//!   the (undirected) centrality/clustering.
//! * **Analytics are privileged, answers are scoped:** analysis runs over the
//!   whole graph; results are filtered to the caller's [`VisibilityScope`] so a
//!   discovery answer never reveals a node the caller could not see normally.

pub mod cache;
#[cfg(feature = "codegraph")]
pub mod codegraph;
pub mod graph;
pub mod influence;
pub mod model;
pub mod org;
pub mod portfolio;
pub mod scope;

pub use cache::{CacheStats, DiscoveryCache};
pub use graph::DiscoveryGraph;
pub use model::{
    CompanyRef, CrossSegmentLink, InfluenceRank, ManageScope, OrgNeighborhood, PersonRef,
    ReportingPath, SegmentReport, SpanMetrics, Stakeholder,
};
pub use scope::{ProjectScope, VisibilityScope};

use kanbrick_auth::AuditLog;
use kanbrick_core::{FirmContext, Result};
use kanbrick_store::Store;

/// Audit marker recorded for a privileged full-graph discovery load.
const DISCOVERY_LOAD_MARKER: &str = "kanbrick-discovery::full-graph-load";

/// Discovery engine over the firm graph.
///
/// Holds the firm graph loaded into graphify-rs (the in-memory copy decided in
/// ADR-0003). Construct it once per graph snapshot with [`from_store`]; the
/// analysis methods (defined across the [`org`], [`portfolio`], [`influence`],
/// and [`scope`] modules) read from it.
///
/// [`from_store`]: DiscoveryEngine::from_store
#[derive(Debug)]
pub struct DiscoveryEngine {
    graph: DiscoveryGraph,
}

impl DiscoveryEngine {
    /// Load the firm graph from `store`.
    ///
    /// This is a **privileged, full-graph read** (graph analytics are only
    /// meaningful over the whole graph). Per-caller clearance filtering is then
    /// applied to *results* via the scoped methods in [`scope`]. When the load
    /// is performed on behalf of a caller, prefer [`from_store_audited`] so the
    /// privileged read is recorded.
    ///
    /// [`from_store_audited`]: DiscoveryEngine::from_store_audited
    pub fn from_store(store: &Store) -> Result<Self> {
        Ok(DiscoveryEngine {
            graph: DiscoveryGraph::from_store(store)?,
        })
    }

    /// Like [`from_store`](Self::from_store) but records an audit entry for the
    /// privileged full-graph load under `loader`'s identity first (handoff §5).
    pub fn from_store_audited(store: &Store, loader: &FirmContext) -> Result<Self> {
        AuditLog::new(store).record(loader, DISCOVERY_LOAD_MARKER)?;
        Self::from_store(store)
    }

    /// Borrow the loaded firm graph.
    pub fn graph(&self) -> &DiscoveryGraph {
        &self.graph
    }

    /// Whether the engine has a loaded, non-empty graph.
    pub fn is_ready(&self) -> bool {
        self.graph.node_count() > 0
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Shared seed-backed test fixtures (mirrors `kanbrick-auth`'s pattern).

    use kanbrick_core::{ClearanceLevel, FirmContext};
    use kanbrick_store::{Migrator, Store};
    use tempfile::TempDir;

    /// Open a fresh store and load the firm seed data into it.
    pub(crate) fn seeded_store() -> (TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let seed = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../seed/kanbrick_seed_data.cypher"
        ))
        .unwrap();
        Migrator::firm(seed).run(&store).unwrap();
        (dir, store)
    }

    /// Build a [`FirmContext`] for `email` at `clearance`.
    pub(crate) fn ctx(email: &str, clearance: ClearanceLevel) -> FirmContext {
        FirmContext::new(uuid::Uuid::new_v4(), email, clearance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanbrick_core::ClearanceLevel;
    use test_support::{ctx, seeded_store};

    #[test]
    fn engine_loads_and_is_ready() {
        let (_d, store) = seeded_store();
        let engine = DiscoveryEngine::from_store(&store).unwrap();
        assert!(engine.is_ready());
        assert_eq!(engine.graph().node_count(), 25);
    }

    #[test]
    fn audited_load_records_an_entry() {
        let (_d, store) = seeded_store();
        let loader = ctx("service:discovery", ClearanceLevel::L5);
        let engine = DiscoveryEngine::from_store_audited(&store, &loader).unwrap();
        assert!(engine.is_ready());

        let audit = AuditLog::new(&store);
        assert_eq!(audit.count_for_user(loader.user_id).unwrap(), 1);
    }
}
