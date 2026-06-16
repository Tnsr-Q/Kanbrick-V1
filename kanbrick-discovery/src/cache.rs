//! Discovery result cache (issue #37).
//!
//! Caches discovery answers keyed by `(function, args, scope identity)` with a
//! configurable TTL. The key **includes the caller's
//! [`scope_key`](crate::scope::VisibilityScope::scope_key)**, so an entry
//! computed for one clearance/scope can never be served to another (an L3 entry
//! never serves an L5 caller). Graph mutations invalidate the cache by bumping a
//! generation counter; entries tagged with an older generation miss.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kanbrick_core::{Error, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::scope::VisibilityScope;

/// Build a cache key for a discovery call: `function(args)@<scope identity>`.
pub fn scoped_key(scope: &dyn VisibilityScope, function: &str, args: &str) -> String {
    format!("{function}({args})@{}", scope.scope_key())
}

/// A point-in-time snapshot of cache counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    /// Lookups served from a live cache entry.
    pub hits: u64,
    /// Lookups that had to compute (absent, expired, or stale generation).
    pub misses: u64,
    /// Live entries currently stored.
    pub entries: usize,
}

struct Entry {
    value: serde_json::Value,
    inserted: Instant,
    ttl: Duration,
    generation: u64,
}

impl Entry {
    fn is_live(&self, generation: u64) -> bool {
        self.generation == generation && self.inserted.elapsed() <= self.ttl
    }
}

struct Inner {
    generation: u64,
    entries: HashMap<String, Entry>,
}

/// A TTL cache for discovery results, safe to share behind an `Arc`.
#[derive(Debug)]
pub struct DiscoveryCache {
    default_ttl: Duration,
    inner: Mutex<Inner>,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner")
            .field("generation", &self.generation)
            .field("entries", &self.entries.len())
            .finish()
    }
}

impl DiscoveryCache {
    /// A cache whose entries live for `default_ttl`.
    pub fn new(default_ttl: Duration) -> Self {
        DiscoveryCache {
            default_ttl,
            inner: Mutex::new(Inner {
                generation: 0,
                entries: HashMap::new(),
            }),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Return the cached value for `key`, or compute, store, and return it.
    /// Uses the cache's default TTL.
    pub fn get_or_compute<T, F>(&self, key: &str, compute: F) -> Result<T>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Result<T>,
    {
        self.get_or_compute_with_ttl(key, self.default_ttl, compute)
    }

    /// Like [`get_or_compute`](Self::get_or_compute) with an explicit TTL.
    pub fn get_or_compute_with_ttl<T, F>(&self, key: &str, ttl: Duration, compute: F) -> Result<T>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Result<T>,
    {
        {
            let mut inner = self.inner.lock().unwrap();
            let generation = inner.generation;
            match inner.entries.get(key) {
                Some(entry) if entry.is_live(generation) => {
                    let value = entry.value.clone();
                    drop(inner);
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    return serde_json::from_value(value)
                        .map_err(|e| Error::Internal(format!("cache decode failed: {e}")));
                }
                Some(_) => {
                    // Expired or stale generation: drop it so `entries` is honest.
                    inner.entries.remove(key);
                }
                None => {}
            }
        }

        self.misses.fetch_add(1, Ordering::Relaxed);
        let computed = compute()?;
        let value = serde_json::to_value(&computed)
            .map_err(|e| Error::Internal(format!("cache encode failed: {e}")))?;
        let mut inner = self.inner.lock().unwrap();
        let generation = inner.generation;
        inner.entries.insert(
            key.to_string(),
            Entry {
                value,
                inserted: Instant::now(),
                ttl,
                generation,
            },
        );
        Ok(computed)
    }

    /// Invalidate every entry (call after a graph mutation). Subsequent lookups
    /// recompute.
    pub fn invalidate_all(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.generation = inner.generation.saturating_add(1);
        inner.entries.clear();
    }

    /// Drop expired entries, returning how many were removed.
    pub fn prune_expired(&self) -> usize {
        let mut inner = self.inner.lock().unwrap();
        let generation = inner.generation;
        let before = inner.entries.len();
        inner.entries.retain(|_, e| e.is_live(generation));
        before - inner.entries.len()
    }

    /// Current cache counters.
    pub fn stats(&self) -> CacheStats {
        let inner = self.inner.lock().unwrap();
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            entries: inner.entries.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{scoped_key, DiscoveryCache};
    use crate::scope::VisibilityScope;
    use kanbrick_core::Result;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    /// A minimal scope whose identity we control, for key tests.
    #[derive(Debug)]
    struct MockScope {
        key: String,
    }
    impl VisibilityScope for MockScope {
        fn sees_all(&self) -> bool {
            false
        }
        fn can_see_person(&self, _: &str) -> bool {
            true
        }
        fn can_see_company(&self, _: &str) -> bool {
            true
        }
        fn scope_key(&self) -> String {
            self.key.clone()
        }
    }

    fn count_call(counter: &AtomicU64) -> Result<u64> {
        Ok(counter.fetch_add(1, Ordering::Relaxed))
    }

    #[test]
    fn second_identical_lookup_is_a_hit() {
        let cache = DiscoveryCache::new(Duration::from_secs(60));
        let calls = AtomicU64::new(0);

        let _: u64 = cache.get_or_compute("k", || count_call(&calls)).unwrap();
        let _: u64 = cache.get_or_compute("k", || count_call(&calls)).unwrap();

        // Computed exactly once; the second lookup hit.
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.entries, 1);
    }

    #[test]
    fn clearance_is_respected_via_the_key() {
        let cache = DiscoveryCache::new(Duration::from_secs(60));
        let l3 = MockScope {
            key: "clearance:L3:x".into(),
        };
        let l5 = MockScope {
            key: "clearance:L5:y".into(),
        };

        let k3 = scoped_key(&l3, "company_stakeholders", "JMTS");
        let k5 = scoped_key(&l5, "company_stakeholders", "JMTS");
        assert_ne!(k3, k5);

        let calls = AtomicU64::new(0);
        let _: u64 = cache.get_or_compute(&k3, || count_call(&calls)).unwrap();
        // The L5 caller has a different key → a miss, not the L3 entry.
        let _: u64 = cache.get_or_compute(&k5, || count_call(&calls)).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn ttl_expiry_removes_the_entry() {
        let cache = DiscoveryCache::new(Duration::from_millis(15));
        let calls = AtomicU64::new(0);

        let _: u64 = cache.get_or_compute("k", || count_call(&calls)).unwrap();
        assert_eq!(cache.stats().entries, 1);

        std::thread::sleep(Duration::from_millis(30));
        // Expired: a fresh compute, and the stale entry is dropped on access.
        let _: u64 = cache.get_or_compute("k", || count_call(&calls)).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn mutation_invalidates_then_recomputes() {
        let cache = DiscoveryCache::new(Duration::from_secs(60));
        let calls = AtomicU64::new(0);

        // query → hit → mutate → miss → fresh.
        let _: u64 = cache.get_or_compute("k", || count_call(&calls)).unwrap();
        let _: u64 = cache.get_or_compute("k", || count_call(&calls)).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1); // one compute, one hit

        cache.invalidate_all();
        assert_eq!(cache.stats().entries, 0);

        let _: u64 = cache.get_or_compute("k", || count_call(&calls)).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2); // recomputed after mutation
    }
}
