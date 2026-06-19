//! Per-guest admission control for synchronous guest invocations (#63, Track A).
//!
//! The HTTP guest path is request/response and runs the WASM call on a blocking
//! thread, so backpressure belongs here at the async edge rather than in the mesh
//! [`Scheduler`](kanbrick_mesh::Scheduler) (which is fire-and-forget and uses a
//! blocking semaphore). Each guest gets two `tokio` semaphores:
//!
//! * **concurrency** — bounds how many invocations of a guest run at once. A
//!   permit is held for the whole run. This is the saturation the autoscaler
//!   reads as `kanbrick_mesh_pressure_ratio`.
//! * **queue** — bounds the total in-system work (running + waiting) per guest. A
//!   permit is held from admission until the run finishes; when it is exhausted,
//!   new requests are rejected with `429 Too Many Requests` instead of piling up
//!   unboundedly.
//!
//! Counters (`queued`, `rejected`) are tracked here; the run/terminal counters
//! live in the mesh core so the [`Scheduler`](kanbrick_mesh::Scheduler) path is
//! counted too. `/metrics` joins both.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Admission tuning. Defaults mirror the mesh [`Scheduler`](kanbrick_mesh::Scheduler)'s
/// per-guest concurrency so the two paths agree.
#[derive(Debug, Clone, Copy)]
pub struct AdmissionConfig {
    /// Maximum guest invocations running concurrently, per guest.
    pub guest_concurrency: usize,
    /// Maximum invocations in the system (running + queued) per guest before new
    /// ones are rejected with `429`.
    pub queue_limit: usize,
}

impl Default for AdmissionConfig {
    fn default() -> Self {
        AdmissionConfig {
            guest_concurrency: 4,
            queue_limit: 32,
        }
    }
}

/// Per-guest admission state.
struct GuestSlot {
    /// Bounds concurrent executions; a permit is held for the duration of a run.
    concurrency: Arc<Semaphore>,
    /// Bounds total in-system (running + queued) work; a permit is held from
    /// admission until the run finishes. Exhaustion ⇒ reject (`429`).
    queue: Arc<Semaphore>,
    /// Concurrency capacity, retained for the pressure ratio.
    capacity: usize,
    /// Invocations admitted but still waiting for a concurrency permit.
    queued: AtomicI64,
    /// Invocations rejected because the queue was full.
    rejected: AtomicU64,
}

impl GuestSlot {
    fn new(config: AdmissionConfig) -> Self {
        // A zero-permit semaphore would deadlock; the queue must be at least as
        // deep as the concurrency or admitted work could never all run.
        let capacity = config.guest_concurrency.max(1);
        let queue = config.queue_limit.max(capacity);
        GuestSlot {
            concurrency: Arc::new(Semaphore::new(capacity)),
            queue: Arc::new(Semaphore::new(queue)),
            capacity,
            queued: AtomicI64::new(0),
            rejected: AtomicU64::new(0),
        }
    }
}

/// Decrements a guest's `queued` gauge on drop, so an invocation whose future is
/// cancelled mid-wait (e.g. the client disconnects) is still uncounted.
struct QueuedGuard(Arc<GuestSlot>);

impl QueuedGuard {
    fn enter(slot: Arc<GuestSlot>) -> Self {
        slot.queued.fetch_add(1, Ordering::Relaxed);
        QueuedGuard(slot)
    }
}

impl Drop for QueuedGuard {
    fn drop(&mut self) {
        self.0.queued.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A grant to run one invocation. Hold it for the duration of the call; dropping
/// it releases the guest's concurrency and queue slots.
pub struct Permit {
    _queue: OwnedSemaphorePermit,
    _concurrency: OwnedSemaphorePermit,
}

/// A point-in-time snapshot of one guest's admission counters (#63, Track A).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionMetric {
    /// The guest's name.
    pub name: String,
    /// Invocations admitted but waiting for a concurrency permit.
    pub queued: i64,
    /// Invocations rejected for overload.
    pub rejected: u64,
    /// Concurrency permits currently held (i.e. running).
    pub in_use: u64,
    /// Concurrency capacity.
    pub capacity: usize,
}

/// Per-guest admission control shared across requests.
pub struct GuestAdmission {
    config: AdmissionConfig,
    slots: RwLock<HashMap<String, Arc<GuestSlot>>>,
}

impl GuestAdmission {
    /// Build admission control, pre-seeding a slot per known guest so they all
    /// appear in `/metrics` from boot. Slots for guests registered later (e.g. via
    /// the Track C registry) are created lazily on first use.
    pub fn new(guests: impl IntoIterator<Item = String>, config: AdmissionConfig) -> Self {
        let slots = guests
            .into_iter()
            .map(|name| (name, Arc::new(GuestSlot::new(config))))
            .collect();
        GuestAdmission {
            config,
            slots: RwLock::new(slots),
        }
    }

    /// The slot for `name`, creating it on first use. The lock is never held
    /// across an `.await`.
    fn slot(&self, name: &str) -> Arc<GuestSlot> {
        if let Some(slot) = self.slots.read().expect("admission lock").get(name) {
            return Arc::clone(slot);
        }
        Arc::clone(
            self.slots
                .write()
                .expect("admission lock")
                .entry(name.to_string())
                .or_insert_with(|| Arc::new(GuestSlot::new(self.config))),
        )
    }

    /// Acquire admission for one invocation of `name`. Returns `Some(permit)` to
    /// run (hold it until the call finishes), or `None` if the guest is overloaded
    /// — the caller should respond `429`.
    pub async fn admit(&self, name: &str) -> Option<Permit> {
        let slot = self.slot(name);
        // Reserve a queue slot first; full queue ⇒ shed load immediately.
        let queue = match Arc::clone(&slot.queue).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                slot.rejected.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        };
        // Now wait (if needed) for a concurrency permit, counted as queued.
        let queued = QueuedGuard::enter(Arc::clone(&slot));
        let concurrency = Arc::clone(&slot.concurrency)
            .acquire_owned()
            .await
            .expect("concurrency semaphore is never closed");
        drop(queued);
        Some(Permit {
            _queue: queue,
            _concurrency: concurrency,
        })
    }

    /// A snapshot of every tracked guest's admission counters, sorted by name.
    pub fn snapshot(&self) -> Vec<AdmissionMetric> {
        let mut out: Vec<AdmissionMetric> = self
            .slots
            .read()
            .expect("admission lock")
            .iter()
            .map(|(name, slot)| AdmissionMetric {
                name: name.clone(),
                queued: slot.queued.load(Ordering::Relaxed),
                rejected: slot.rejected.load(Ordering::Relaxed),
                in_use: (slot.capacity as u64)
                    .saturating_sub(slot.concurrency.available_permits() as u64),
                capacity: slot.capacity,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(snap: &'a [AdmissionMetric], name: &str) -> &'a AdmissionMetric {
        snap.iter()
            .find(|m| m.name == name)
            .expect("metric present")
    }

    #[tokio::test]
    async fn admits_then_rejects_when_queue_is_full() {
        let adm = GuestAdmission::new(
            std::iter::once("g".to_string()),
            AdmissionConfig {
                guest_concurrency: 1,
                queue_limit: 1,
            },
        );

        let permit = adm.admit("g").await.expect("first invocation admitted");
        let snap = adm.snapshot();
        assert_eq!(find(&snap, "g").in_use, 1);
        assert_eq!(find(&snap, "g").capacity, 1);

        // Queue depth is 1 and it is occupied, so the next request sheds.
        assert!(
            adm.admit("g").await.is_none(),
            "overloaded guest rejects further work"
        );
        assert_eq!(find(&adm.snapshot(), "g").rejected, 1);

        // Releasing the permit frees the slot for the next caller.
        drop(permit);
        assert!(
            adm.admit("g").await.is_some(),
            "slot reusable after release"
        );
        assert_eq!(
            find(&adm.snapshot(), "g").rejected,
            1,
            "rejections are not double-counted"
        );
    }

    #[tokio::test]
    async fn unknown_guest_gets_a_lazily_created_slot() {
        let adm = GuestAdmission::new(std::iter::empty(), AdmissionConfig::default());
        assert!(adm.admit("late").await.is_some());
        assert_eq!(find(&adm.snapshot(), "late").capacity, 4);
    }
}
