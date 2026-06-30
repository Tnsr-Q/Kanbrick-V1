//! Memoized firm org-graph on [`AppState`](crate::AppState).
//!
//! [`DiscoveryGraph::from_store`](kanbrick_discovery::DiscoveryGraph::from_store)
//! is a privileged full-graph read — it pulls every Person/Company/Segment plus the
//! reporting/management edges out of SparrowDB and builds one in-memory graphify
//! graph (ADR-0003). The grantor-gated handlers (scope approve/deny, skill review)
//! each need that graph to re-check the requester's management chain, and until now
//! each rebuilt it from scratch per request.
//!
//! The firm's org structure (people, reporting, management) is static between
//! reorgs — no current write path mutates it — so [`OrgGraphCache`] builds the graph
//! once and hands every grantor action the same `Arc`, rebuilding only after a TTL
//! (a cheap bound on staleness from an out-of-band store change such as a seed
//! reload) or an explicit [`invalidate`](OrgGraphCache::invalidate) (the hook a
//! future hiring/reorg endpoint calls).
//!
//! The graph is privileged and **identical for every caller**, so it is *not* keyed
//! by clearance — per-caller filtering happens on the analytics *results* (the scoped
//! methods in `kanbrick_discovery::scope`), so sharing one graph leaks nothing.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kanbrick_core::Result;
use kanbrick_discovery::DiscoveryGraph;
use kanbrick_store::Store;

/// Default freshness window for the memoized org-graph. Long enough that a burst of
/// grantor actions reuses one build, short enough that an out-of-band store change is
/// picked up without a process restart.
pub const DEFAULT_ORG_GRAPH_TTL: Duration = Duration::from_secs(300);

/// One memoized build: the shared graph plus when it was loaded.
struct Cached {
    graph: Arc<DiscoveryGraph>,
    built: Instant,
}

/// A TTL-bounded memo of the firm org-graph, cheap to clone and safe to share across
/// the async runtime (the build is serialized behind a `Mutex`). Lives on
/// [`AppState`](crate::AppState).
#[derive(Clone)]
pub struct OrgGraphCache {
    ttl: Duration,
    inner: Arc<Mutex<Option<Cached>>>,
}

impl OrgGraphCache {
    /// A memo whose built graph is reused for `ttl` before the next read rebuilds.
    pub fn new(ttl: Duration) -> Self {
        OrgGraphCache {
            ttl,
            inner: Arc::new(Mutex::new(None)),
        }
    }

    /// The memoized org-graph, rebuilt from `store` when absent or older than the
    /// TTL. The rebuild runs under the lock, so a burst of concurrent grantor actions
    /// loads the graph once rather than once per request.
    pub fn get_or_load(&self, store: &Store) -> Result<Arc<DiscoveryGraph>> {
        let mut guard = self.inner.lock().expect("org-graph memo lock poisoned");
        if let Some(cached) = guard.as_ref() {
            if cached.built.elapsed() <= self.ttl {
                return Ok(cached.graph.clone());
            }
        }
        let graph = Arc::new(DiscoveryGraph::from_store(store)?);
        *guard = Some(Cached {
            graph: graph.clone(),
            built: Instant::now(),
        });
        Ok(graph)
    }

    /// Drop the memoized graph so the next read rebuilds — the hook a write that
    /// changes the firm's people/reporting/management structure calls.
    pub fn invalidate(&self) {
        *self.inner.lock().expect("org-graph memo lock poisoned") = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanbrick_store::Migrator;

    /// Open a fresh store and load the firm seed (the same fixture the grant tests
    /// use), so `from_store` has a real org chart to build.
    fn seeded_store() -> (tempfile::TempDir, Store) {
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

    #[test]
    fn second_load_is_memoized() {
        let (_dir, store) = seeded_store();
        let memo = OrgGraphCache::new(Duration::from_secs(300));
        let a = memo.get_or_load(&store).unwrap();
        let b = memo.get_or_load(&store).unwrap();
        // Built once and reused: same allocation, and a non-empty graph.
        assert!(Arc::ptr_eq(&a, &b));
        assert!(a.node_count() > 0);
    }

    #[test]
    fn invalidate_forces_a_rebuild() {
        let (_dir, store) = seeded_store();
        let memo = OrgGraphCache::new(Duration::from_secs(300));
        let a = memo.get_or_load(&store).unwrap();
        memo.invalidate();
        let b = memo.get_or_load(&store).unwrap();
        // Invalidation dropped the memo, so the next read built a fresh graph.
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn ttl_expiry_rebuilds() {
        let (_dir, store) = seeded_store();
        let memo = OrgGraphCache::new(Duration::from_millis(20));
        let a = memo.get_or_load(&store).unwrap();
        std::thread::sleep(Duration::from_millis(45));
        let b = memo.get_or_load(&store).unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
    }

    /// The memo must stay `Send + Sync` so it can live in the axum-shared `AppState`.
    /// A pointed failure here beats a confusing `State` trait error elsewhere.
    #[test]
    fn memo_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<OrgGraphCache>();
    }
}
