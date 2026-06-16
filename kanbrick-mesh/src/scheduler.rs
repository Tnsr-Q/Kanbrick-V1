//! Task scheduler: immediate dispatch with a wall-clock timeout and a per-guest
//! concurrency limit (issue #25).
//!
//! [`Scheduler`] wraps a [`MeshRuntime`] and runs guest invocations as background
//! tasks. Each [`schedule`](Scheduler::schedule) returns a unique [`TaskId`]
//! immediately; the task's [`TaskStatus`] is queryable as it moves
//! `Queued → Running → {Completed, TimedOut, Failed}`.
//!
//! * **Timeout** — a task's wall-clock budget is enforced with wasmtime *epoch
//!   interruption*. The scheduler runs one background thread that increments the
//!   engine epoch every [`tick`](SchedulerConfig::tick); a task that outruns its
//!   budget is killed and reported [`TaskStatus::TimedOut`] (ADR-0002 / #25).
//! * **Concurrency** — at most [`per_guest_concurrency`](SchedulerConfig::per_guest_concurrency)
//!   tasks run for any one guest at a time; excess tasks queue (a per-guest
//!   counting semaphore) and start as slots free up.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use kanbrick_core::abi::{Event, GuestRequest, GuestResponse};
use kanbrick_core::FirmContext;

use crate::error::MeshError;
use crate::event::{EventBus, SubscriptionId};
use crate::MeshRuntime;

/// A unique handle to a scheduled task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(u64);

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "task-{}", self.0)
    }
}

/// The lifecycle state of a scheduled task.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    /// Submitted, waiting for a per-guest concurrency slot.
    Queued,
    /// Currently executing in the runtime.
    Running,
    /// Finished successfully with this response.
    Completed(GuestResponse),
    /// Killed for exceeding its wall-clock budget (#25).
    TimedOut,
    /// Finished with an error (trap, resource limit, bad output, …).
    Failed(String),
}

impl TaskStatus {
    /// Whether this is a final state (no further transitions).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskStatus::Completed(_) | TaskStatus::TimedOut | TaskStatus::Failed(_)
        )
    }
}

/// Scheduler tuning.
#[derive(Debug, Clone, Copy)]
pub struct SchedulerConfig {
    /// Max concurrently-running tasks per guest; excess tasks queue.
    pub per_guest_concurrency: usize,
    /// Granularity of the epoch ticker that drives timeouts. A task timeout is
    /// rounded up to a whole number of these ticks.
    pub tick: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        SchedulerConfig {
            per_guest_concurrency: 4,
            tick: Duration::from_millis(10),
        }
    }
}

/// Exponential-backoff retry policy for a scheduled task (#26).
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Maximum retries after the first attempt (`0` = no retry).
    pub max_retries: u32,
    /// Delay before the first retry.
    pub base_delay: Duration,
    /// Multiplier applied to the delay after each retry.
    pub factor: u32,
}

impl RetryPolicy {
    /// No retries.
    pub fn none() -> Self {
        RetryPolicy {
            max_retries: 0,
            base_delay: Duration::ZERO,
            factor: 1,
        }
    }

    /// Retry up to `max_retries` times, starting at `base_delay` and multiplying
    /// by `factor` each time.
    pub fn exponential(max_retries: u32, base_delay: Duration, factor: u32) -> Self {
        RetryPolicy {
            max_retries,
            base_delay,
            factor,
        }
    }

    /// The backoff delay before retry number `retry_index` (0-based).
    fn backoff(&self, retry_index: u32) -> Duration {
        let multiplier = self.factor.checked_pow(retry_index).unwrap_or(u32::MAX);
        self.base_delay
            .checked_mul(multiplier)
            .unwrap_or(Duration::MAX)
    }
}

/// A cancellable handle to a recurring or event-driven trigger (#26). Cancelling
/// (or dropping) it stops the trigger from scheduling further tasks.
pub struct TriggerHandle {
    stop: Arc<AtomicBool>,
    fired: Arc<AtomicU64>,
    join: Mutex<Option<JoinHandle<()>>>,
    unsubscribe: Mutex<Option<(EventBus, SubscriptionId)>>,
}

impl TriggerHandle {
    /// Stop the trigger: it schedules no further tasks. Idempotent. Tasks already
    /// scheduled run to completion.
    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some((bus, id)) = self.unsubscribe.lock().expect("trigger lock").take() {
            bus.unsubscribe(id);
        }
        if let Some(join) = self.join.lock().expect("trigger lock").take() {
            let _ = join.join();
        }
    }

    /// How many tasks this trigger has scheduled so far.
    pub fn fired(&self) -> u64 {
        self.fired.load(Ordering::Relaxed)
    }
}

impl Drop for TriggerHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// A counting semaphore (blocking `acquire`, non-blocking `release`).
struct Semaphore {
    permits: Mutex<usize>,
    available: Condvar,
}

impl Semaphore {
    fn new(permits: usize) -> Self {
        Semaphore {
            permits: Mutex::new(permits),
            available: Condvar::new(),
        }
    }

    fn acquire(&self) {
        let mut permits = self.permits.lock().expect("semaphore lock");
        while *permits == 0 {
            permits = self.available.wait(permits).expect("semaphore wait");
        }
        *permits -= 1;
    }

    fn release(&self) {
        let mut permits = self.permits.lock().expect("semaphore lock");
        *permits += 1;
        self.available.notify_one();
    }
}

/// State shared between the scheduler and its worker threads.
struct Shared {
    statuses: Mutex<HashMap<TaskId, TaskStatus>>,
    status_changed: Condvar,
    sems: Mutex<HashMap<String, Arc<Semaphore>>>,
    per_guest: usize,
    next_id: AtomicU64,
}

impl Shared {
    fn set_status(&self, id: TaskId, status: TaskStatus) {
        self.statuses
            .lock()
            .expect("status lock")
            .insert(id, status);
        self.status_changed.notify_all();
    }

    fn guest_semaphore(&self, name: &str) -> Arc<Semaphore> {
        self.sems
            .lock()
            .expect("semaphore map lock")
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(self.per_guest)))
            .clone()
    }
}

/// Runs guest invocations as background tasks with timeouts and per-guest
/// concurrency limits. See the [module docs](self).
pub struct Scheduler {
    runtime: Arc<MeshRuntime>,
    shared: Arc<Shared>,
    tick: Duration,
    stop: Arc<AtomicBool>,
    ticker: Option<JoinHandle<()>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl Scheduler {
    /// Create a scheduler over `runtime` with the default [`SchedulerConfig`].
    pub fn new(runtime: Arc<MeshRuntime>) -> Self {
        Self::with_config(runtime, SchedulerConfig::default())
    }

    /// Create a scheduler with explicit configuration. Spawns the epoch ticker.
    pub fn with_config(runtime: Arc<MeshRuntime>, config: SchedulerConfig) -> Self {
        let shared = Arc::new(Shared {
            statuses: Mutex::new(HashMap::new()),
            status_changed: Condvar::new(),
            sems: Mutex::new(HashMap::new()),
            per_guest: config.per_guest_concurrency.max(1),
            next_id: AtomicU64::new(0),
        });
        let stop = Arc::new(AtomicBool::new(false));

        // The epoch ticker: advancing the engine epoch is what makes a task's
        // epoch deadline fire, killing an overrunning guest.
        let engine = runtime.engine().clone();
        let tick = config.tick.max(Duration::from_millis(1));
        let ticker_stop = stop.clone();
        let ticker = thread::spawn(move || {
            while !ticker_stop.load(Ordering::Relaxed) {
                thread::sleep(tick);
                engine.increment_epoch();
            }
        });

        Scheduler {
            runtime,
            shared,
            tick,
            stop,
            ticker: Some(ticker),
            workers: Mutex::new(Vec::new()),
        }
    }

    /// Schedule `request` for guest `name` on behalf of `ctx`, returning a
    /// [`TaskId`] immediately. `timeout` bounds the guest's wall-clock execution
    /// (`None` = unbounded). The task queues if the guest is at its concurrency
    /// limit, then runs in the background.
    pub fn schedule(
        &self,
        name: &str,
        ctx: &FirmContext,
        request: &GuestRequest,
        timeout: Option<Duration>,
    ) -> TaskId {
        self.schedule_with_retry(name, ctx, request, timeout, RetryPolicy::none())
    }

    /// Like [`schedule`](Self::schedule) but a failed attempt is retried with
    /// exponential backoff per `retry` (#26). The task only becomes `Failed` /
    /// `TimedOut` after the retries are exhausted.
    pub fn schedule_with_retry(
        &self,
        name: &str,
        ctx: &FirmContext,
        request: &GuestRequest,
        timeout: Option<Duration>,
        retry: RetryPolicy,
    ) -> TaskId {
        let id = TaskId(self.shared.next_id.fetch_add(1, Ordering::Relaxed));
        self.shared.set_status(id, TaskStatus::Queued);

        let runtime = self.runtime.clone();
        let shared = self.shared.clone();
        let semaphore = self.shared.guest_semaphore(name);
        let deadline = self.deadline_ticks(timeout);
        let name = name.to_string();
        let ctx = ctx.clone();
        let request = request.clone();

        let worker = thread::spawn(move || {
            // Blocking here is the queue: a task waits until the guest is under
            // its per-guest concurrency limit.
            semaphore.acquire();

            let mut attempt = 0u32;
            let final_status = loop {
                shared.set_status(id, TaskStatus::Running);
                match runtime.invoke_with_deadline(&name, &ctx, &request, deadline) {
                    Ok(response) => break TaskStatus::Completed(response),
                    Err(e) => {
                        let status = match e {
                            MeshError::Timeout { .. } => TaskStatus::TimedOut,
                            other => TaskStatus::Failed(other.to_string()),
                        };
                        if attempt < retry.max_retries {
                            thread::sleep(retry.backoff(attempt));
                            attempt += 1;
                            continue;
                        }
                        break status;
                    }
                }
            };
            shared.set_status(id, final_status);
            semaphore.release();
        });
        self.workers.lock().expect("workers lock").push(worker);
        id
    }

    /// Fire `request` at guest `name` every `interval`, until the returned
    /// [`TriggerHandle`] is cancelled or dropped (#26). Requires an `Arc<Scheduler>`.
    // Trigger registration inherently carries the full task spec plus its source.
    #[allow(clippy::too_many_arguments)]
    pub fn schedule_interval(
        self: &Arc<Self>,
        interval: Duration,
        name: &str,
        ctx: &FirmContext,
        request: &GuestRequest,
        timeout: Option<Duration>,
        retry: RetryPolicy,
    ) -> TriggerHandle {
        let stop = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicU64::new(0));
        let scheduler = self.clone();
        let (loop_stop, loop_fired) = (stop.clone(), fired.clone());
        let (name, ctx, request) = (name.to_string(), ctx.clone(), request.clone());

        let join = thread::spawn(move || {
            while !loop_stop.load(Ordering::Relaxed) {
                thread::sleep(interval);
                if loop_stop.load(Ordering::Relaxed) {
                    break;
                }
                scheduler.schedule_with_retry(&name, &ctx, &request, timeout, retry);
                loop_fired.fetch_add(1, Ordering::Relaxed);
            }
        });

        TriggerHandle {
            stop,
            fired,
            join: Mutex::new(Some(join)),
            unsubscribe: Mutex::new(None),
        }
    }

    /// Fire a task at guest `name` whenever an event of `kind` is published on
    /// `bus`, deriving each task's [`GuestRequest`] from the event via
    /// `make_request` (#26). Cancel or drop the returned [`TriggerHandle`] to
    /// stop. Requires an `Arc<Scheduler>`.
    // Trigger registration inherently carries the full task spec plus its source.
    #[allow(clippy::too_many_arguments)]
    pub fn on_event(
        self: &Arc<Self>,
        bus: &EventBus,
        kind: impl Into<String>,
        make_request: impl Fn(&Event) -> GuestRequest + Send + Sync + 'static,
        name: &str,
        ctx: &FirmContext,
        timeout: Option<Duration>,
        retry: RetryPolicy,
    ) -> TriggerHandle {
        let stop = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicU64::new(0));
        let scheduler = self.clone();
        let (sub_stop, sub_fired) = (stop.clone(), fired.clone());
        let (name, ctx) = (name.to_string(), ctx.clone());

        let id = bus.subscribe(kind, move |event| {
            if sub_stop.load(Ordering::Relaxed) {
                return;
            }
            let request = make_request(event);
            scheduler.schedule_with_retry(&name, &ctx, &request, timeout, retry);
            sub_fired.fetch_add(1, Ordering::Relaxed);
        });

        TriggerHandle {
            stop,
            fired,
            join: Mutex::new(None),
            unsubscribe: Mutex::new(Some((bus.clone(), id))),
        }
    }

    /// The current status of `id`, or `None` if no such task exists.
    pub fn status(&self, id: TaskId) -> Option<TaskStatus> {
        self.shared
            .statuses
            .lock()
            .expect("status lock")
            .get(&id)
            .cloned()
    }

    /// Block until `id` reaches a terminal status or `timeout` elapses. Returns
    /// the terminal status, or `None` on timeout / unknown id.
    pub fn wait(&self, id: TaskId, timeout: Duration) -> Option<TaskStatus> {
        let deadline = Instant::now() + timeout;
        let mut statuses = self.shared.statuses.lock().expect("status lock");
        loop {
            match statuses.get(&id) {
                Some(status) if status.is_terminal() => return Some(status.clone()),
                Some(_) => {}
                None => return None,
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let (next, timed_out) = self
                .shared
                .status_changed
                .wait_timeout(statuses, remaining)
                .expect("status wait");
            statuses = next;
            if timed_out.timed_out() {
                // Re-check once more, then give up on the next loop iteration.
            }
        }
    }

    /// Convert a task timeout into a whole number of engine-epoch ticks.
    fn deadline_ticks(&self, timeout: Option<Duration>) -> u64 {
        match timeout {
            None => u64::MAX,
            Some(d) => {
                let tick = self.tick.as_nanos().max(1);
                let ticks = d.as_nanos().div_ceil(tick);
                u64::try_from(ticks).unwrap_or(u64::MAX).max(1)
            }
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        // Stop the ticker, then drain outstanding work (each running task is
        // bounded by its timeout, so this terminates).
        self.stop.store(true, Ordering::Relaxed);
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
        for worker in self.workers.lock().expect("workers lock").drain(..) {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeLimits;
    use kanbrick_core::ClearanceLevel;
    use serde_json::json;
    use uuid::Uuid;

    /// Hermetic echo guest (returns input unchanged).
    const ECHO_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (global $next (mut i32) (i32.const 1024))
          (func (export "kbk_alloc") (param $len i32) (result i32)
            (local $p i32)
            global.get $next local.set $p
            global.get $next local.get $len i32.add global.set $next
            local.get $p)
          (func (export "kbk_run") (param $ptr i32) (param $len i32) (result i64)
            local.get $ptr i64.extend_i32_u i64.const 32 i64.shl
            local.get $len i64.extend_i32_u i64.or))
    "#;

    /// A guest that spins ~300M iterations, then echoes its input. Long enough to
    /// observe queuing; finishes (so it Completes rather than timing out) when
    /// given unbounded time and ample fuel.
    const BUSY_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (global $next (mut i32) (i32.const 1024))
          (func (export "kbk_alloc") (param $len i32) (result i32)
            (local $p i32)
            global.get $next local.set $p
            global.get $next local.get $len i32.add global.set $next
            local.get $p)
          (func (export "kbk_run") (param $ptr i32) (param $len i32) (result i64)
            (local $i i32)
            (block $done
              (loop $l
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br_if $done (i32.ge_u (local.get $i) (i32.const 300000000)))
                (br $l)))
            local.get $ptr i64.extend_i32_u i64.const 32 i64.shl
            local.get $len i64.extend_i32_u i64.or))
    "#;

    /// A guest that loops forever — only a timeout (or fuel) stops it.
    const SPIN_FOREVER_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (global $next (mut i32) (i32.const 1024))
          (func (export "kbk_alloc") (param $len i32) (result i32)
            (local $p i32)
            global.get $next local.set $p
            global.get $next local.get $len i32.add global.set $next
            local.get $p)
          (func (export "kbk_run") (param $ptr i32) (param $len i32) (result i64)
            (loop $l (br $l))
            (i64.const 0)))
    "#;

    /// A guest whose `kbk_run` always traps — every attempt fails.
    const ALWAYS_FAILS_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (global $next (mut i32) (i32.const 1024))
          (func (export "kbk_alloc") (param $len i32) (result i32)
            (local $p i32)
            global.get $next local.set $p
            global.get $next local.get $len i32.add global.set $next
            local.get $p)
          (func (export "kbk_run") (param $ptr i32) (param $len i32) (result i64)
            unreachable))
    "#;

    fn ctx() -> FirmContext {
        FirmContext::new(Uuid::new_v4(), "u@kanbrick.com", ClearanceLevel::L3)
    }

    /// A runtime with effectively unlimited fuel, so the epoch deadline (not fuel
    /// exhaustion) is what bounds a guest — keeping the timeout tests about #25.
    fn runtime_with(modules: &[(&str, &str)]) -> Arc<MeshRuntime> {
        let limits = RuntimeLimits {
            fuel: u64::MAX,
            ..RuntimeLimits::default()
        };
        let mut rt = MeshRuntime::with_limits(limits).unwrap();
        for (name, wat) in modules {
            rt.register_module(name, "0.1.0", wat.as_bytes()).unwrap();
        }
        Arc::new(rt)
    }

    #[test]
    fn immediate_dispatch_runs_and_returns_the_response() {
        let sched = Scheduler::new(runtime_with(&[("echo", ECHO_WAT)]));
        let req = GuestRequest::new(json!({"hello": "mesh"}));
        let id = sched.schedule("echo", &ctx(), &req, None);

        let status = sched.wait(id, Duration::from_secs(5)).unwrap();
        match status {
            TaskStatus::Completed(resp) => assert_eq!(resp.payload, json!({"hello": "mesh"})),
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn task_ids_are_unique_and_status_is_queryable() {
        let sched = Scheduler::new(runtime_with(&[("echo", ECHO_WAT)]));
        let req = GuestRequest::new(json!(null));
        let a = sched.schedule("echo", &ctx(), &req, None);
        let b = sched.schedule("echo", &ctx(), &req, None);
        assert_ne!(a, b);
        assert!(sched.status(a).is_some());
        assert!(sched.status(TaskId(9999)).is_none());

        assert!(sched.wait(a, Duration::from_secs(5)).unwrap().is_terminal());
        assert!(sched.wait(b, Duration::from_secs(5)).unwrap().is_terminal());
    }

    #[test]
    fn a_guest_that_overruns_its_timeout_is_killed() {
        let sched = Scheduler::new(runtime_with(&[("spin", SPIN_FOREVER_WAT)]));
        let id = sched.schedule(
            "spin",
            &ctx(),
            &GuestRequest::new(json!(null)),
            Some(Duration::from_millis(50)),
        );
        let status = sched.wait(id, Duration::from_secs(5)).unwrap();
        assert_eq!(status, TaskStatus::TimedOut);
    }

    #[test]
    fn per_guest_concurrency_limit_is_never_exceeded() {
        // Limit of 1: two busy tasks for the same guest must never run at once.
        let config = SchedulerConfig {
            per_guest_concurrency: 1,
            tick: Duration::from_millis(10),
        };
        let sched = Scheduler::with_config(runtime_with(&[("busy", BUSY_WAT)]), config);
        let req = GuestRequest::new(json!(null));
        let a = sched.schedule("busy", &ctx(), &req, None);
        let b = sched.schedule("busy", &ctx(), &req, None);

        // Poll the pair: with a limit of 1 they may never both be Running, and we
        // should observe the second queued behind the first.
        let mut saw_queue_behind_running = false;
        for _ in 0..2000 {
            let (sa, sb) = (sched.status(a), sched.status(b));
            let both_running =
                matches!(sa, Some(TaskStatus::Running)) && matches!(sb, Some(TaskStatus::Running));
            assert!(!both_running, "per-guest concurrency limit of 1 violated");
            // Either worker may win the single permit first; we just need to see
            // one running while the other waits its turn.
            let one_running_one_queued = (matches!(sa, Some(TaskStatus::Running))
                && matches!(sb, Some(TaskStatus::Queued)))
                || (matches!(sb, Some(TaskStatus::Running))
                    && matches!(sa, Some(TaskStatus::Queued)));
            if one_running_one_queued {
                saw_queue_behind_running = true;
            }
            if sched.status(a).map(|s| s.is_terminal()).unwrap_or(false)
                && sched.status(b).map(|s| s.is_terminal()).unwrap_or(false)
            {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            saw_queue_behind_running,
            "expected the second task to queue behind the first"
        );

        // Both still complete successfully.
        assert!(matches!(
            sched.wait(a, Duration::from_secs(10)).unwrap(),
            TaskStatus::Completed(_)
        ));
        assert!(matches!(
            sched.wait(b, Duration::from_secs(10)).unwrap(),
            TaskStatus::Completed(_)
        ));
    }

    #[test]
    fn scheduling_an_unknown_guest_fails_the_task() {
        let sched = Scheduler::new(runtime_with(&[]));
        let id = sched.schedule("nope", &ctx(), &GuestRequest::new(json!(null)), None);
        assert!(matches!(
            sched.wait(id, Duration::from_secs(5)).unwrap(),
            TaskStatus::Failed(_)
        ));
    }

    #[test]
    fn semaphore_blocks_until_a_permit_is_released() {
        let sem = Arc::new(Semaphore::new(1));
        sem.acquire();
        let sem2 = sem.clone();
        let acquired = Arc::new(AtomicBool::new(false));
        let acquired2 = acquired.clone();
        let h = thread::spawn(move || {
            sem2.acquire();
            acquired2.store(true, Ordering::SeqCst);
        });
        thread::sleep(Duration::from_millis(30));
        assert!(!acquired.load(Ordering::SeqCst), "must block while held");
        sem.release();
        h.join().unwrap();
        assert!(acquired.load(Ordering::SeqCst));
    }

    // ---- #26: recurring + event-triggered + retry + cancellation. ----

    #[test]
    fn a_recurring_trigger_fires_on_an_interval_until_cancelled() {
        use std::sync::Arc;
        let sched = Arc::new(Scheduler::new(runtime_with(&[("echo", ECHO_WAT)])));
        let handle = sched.schedule_interval(
            Duration::from_millis(20),
            "echo",
            &ctx(),
            &GuestRequest::new(json!(null)),
            None,
            RetryPolicy::none(),
        );
        // After ~120ms at a 20ms interval, several tasks should have fired.
        thread::sleep(Duration::from_millis(120));
        let fired = handle.fired();
        assert!(
            fired >= 3,
            "expected the trigger to fire repeatedly, got {fired}"
        );

        // Cancelling stops further firing.
        handle.cancel();
        let after_cancel = handle.fired();
        thread::sleep(Duration::from_millis(80));
        assert_eq!(handle.fired(), after_cancel, "cancel must stop the trigger");
    }

    #[test]
    fn an_event_trigger_schedules_a_task_per_matching_event() {
        use std::sync::Arc;
        let sched = Arc::new(Scheduler::new(runtime_with(&[("echo", ECHO_WAT)])));
        let bus = EventBus::new();
        let handle = sched.on_event(
            &bus,
            "do.it",
            |event| GuestRequest::new(event.payload.clone()),
            "echo",
            &ctx(),
            None,
            RetryPolicy::none(),
        );

        for _ in 0..3 {
            bus.emit(Event::with_payload("do.it", json!({"n": 1})));
        }
        // Events of other kinds do not trigger.
        bus.emit(Event::new("ignored"));
        assert_eq!(handle.fired(), 3);

        // After cancelling, new events schedule nothing.
        handle.cancel();
        bus.emit(Event::with_payload("do.it", json!({"n": 2})));
        assert_eq!(handle.fired(), 3);
    }

    #[test]
    fn a_failing_task_is_retried_with_exponential_backoff() {
        let sched = Scheduler::new(runtime_with(&[("boom", ALWAYS_FAILS_WAT)]));
        // 2 retries: backoffs of 25ms then 50ms before the final failure.
        let retry = RetryPolicy::exponential(2, Duration::from_millis(25), 2);
        let started = std::time::Instant::now();
        let id =
            sched.schedule_with_retry("boom", &ctx(), &GuestRequest::new(json!(null)), None, retry);

        let status = sched.wait(id, Duration::from_secs(5)).unwrap();
        assert!(matches!(status, TaskStatus::Failed(_)));
        // The two backoffs (25 + 50 = 75ms) must have elapsed before giving up.
        assert!(
            started.elapsed() >= Duration::from_millis(70),
            "expected exponential backoff to delay the final failure"
        );
    }
}
