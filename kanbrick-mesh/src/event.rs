//! The event bus (issue #27).
//!
//! [`EventBus`] is the host's publish/subscribe fabric. A guest (or the host)
//! [`emit`](EventBus::emit)s an [`Event`]; every subscription whose kind matches
//! is notified. The bus also keeps an ordered, **replayable** log of every event
//! so a late subscriber can catch up and so events with no current subscriber are
//! retained and logged rather than dropped.
//!
//! Subscriptions are *typed* at the edges: [`subscribe_typed`](EventBus::subscribe_typed)
//! deserializes an event's JSON payload into a concrete schema (e.g. a
//! `ValuationComplete { .. }`) before invoking the handler, so a reporting guest
//! can react to a valuation guest's completion with a strongly-typed payload.
//!
//! The log is in-memory: it is the source of truth for the running process and is
//! replayable within it. Durable, cross-restart persistence is a future
//! extension (it would back this same log with the store).

use std::sync::{Arc, Mutex};

use kanbrick_core::abi::Event;
use serde::de::DeserializeOwned;

/// A handle to a subscription, used to [`unsubscribe`](EventBus::unsubscribe).
pub type SubscriptionId = u64;

type Handler = Arc<dyn Fn(&Event) + Send + Sync>;

struct Subscription {
    id: SubscriptionId,
    kind: String,
    handler: Handler,
}

#[derive(Default)]
struct Inner {
    /// Every event ever emitted, in order — the replayable log.
    log: Vec<Event>,
    subscriptions: Vec<Subscription>,
    next_id: SubscriptionId,
}

/// A cloneable, thread-safe publish/subscribe event bus with a replayable log.
#[derive(Clone, Default)]
pub struct EventBus {
    inner: Arc<Mutex<Inner>>,
}

impl EventBus {
    /// Create an empty bus.
    pub fn new() -> Self {
        EventBus::default()
    }

    /// Subscribe `handler` to events of `kind`. Returns a [`SubscriptionId`].
    pub fn subscribe(
        &self,
        kind: impl Into<String>,
        handler: impl Fn(&Event) + Send + Sync + 'static,
    ) -> SubscriptionId {
        let mut inner = self.inner.lock().expect("event bus lock");
        let id = inner.next_id;
        inner.next_id += 1;
        inner.subscriptions.push(Subscription {
            id,
            kind: kind.into(),
            handler: Arc::new(handler),
        });
        id
    }

    /// Subscribe to events of `kind`, deserializing each event's JSON payload
    /// into `T` before invoking `handler`. A payload that does not match `T` is
    /// logged and skipped (the bad event still stays in the replayable log).
    pub fn subscribe_typed<T>(
        &self,
        kind: impl Into<String>,
        handler: impl Fn(T) + Send + Sync + 'static,
    ) -> SubscriptionId
    where
        T: DeserializeOwned,
    {
        let kind = kind.into();
        let kind_for_log = kind.clone();
        self.subscribe(kind, move |event| {
            match serde_json::from_value::<T>(event.payload.clone()) {
                Ok(typed) => handler(typed),
                Err(e) => tracing::warn!(
                    target: "kanbrick_mesh::event",
                    kind = %kind_for_log,
                    error = %e,
                    "dropping event with payload that does not match the subscription schema"
                ),
            }
        })
    }

    /// Remove a subscription. Unknown ids are ignored.
    pub fn unsubscribe(&self, id: SubscriptionId) {
        let mut inner = self.inner.lock().expect("event bus lock");
        inner.subscriptions.retain(|s| s.id != id);
    }

    /// Emit `event`: append it to the replayable log and notify every matching
    /// subscription. Returns how many subscribers were notified. An event with no
    /// subscribers is **logged, not dropped** — it remains in the log for replay.
    pub fn emit(&self, event: Event) -> usize {
        // Collect matching handlers under the lock, then invoke them *outside* it
        // so a handler is free to emit further events without deadlocking.
        let handlers: Vec<Handler> = {
            let mut inner = self.inner.lock().expect("event bus lock");
            inner.log.push(event.clone());
            inner
                .subscriptions
                .iter()
                .filter(|s| s.kind == event.kind)
                .map(|s| s.handler.clone())
                .collect()
        };

        if handlers.is_empty() {
            tracing::info!(
                target: "kanbrick_mesh::event",
                kind = %event.kind,
                "event emitted with no subscribers (retained in the log for replay)"
            );
        }
        for handler in &handlers {
            handler(&event);
        }
        handlers.len()
    }

    /// The full ordered event log.
    pub fn history(&self) -> Vec<Event> {
        self.inner.lock().expect("event bus lock").log.clone()
    }

    /// Replay logged events to `handler`, optionally filtered to one `kind`. Lets
    /// a late subscriber catch up on everything it missed.
    pub fn replay(&self, kind: Option<&str>, handler: impl Fn(&Event)) {
        let events = {
            let inner = self.inner.lock().expect("event bus lock");
            inner
                .log
                .iter()
                .filter(|e| kind.is_none_or(|k| e.kind == k))
                .cloned()
                .collect::<Vec<_>>()
        };
        for event in &events {
            handler(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct ValuationComplete {
        company_id: String,
        npv: f64,
    }

    #[test]
    fn a_typed_subscriber_receives_matching_events() {
        let bus = EventBus::new();
        let received = Arc::new(Mutex::new(Vec::<ValuationComplete>::new()));
        let sink = received.clone();
        bus.subscribe_typed::<ValuationComplete>("valuation.completed", move |v| {
            sink.lock().unwrap().push(v);
        });

        let notified = bus.emit(Event::with_payload(
            "valuation.completed",
            json!({"company_id": "ACME", "npv": 12.5}),
        ));
        assert_eq!(notified, 1);
        assert_eq!(
            received.lock().unwrap().as_slice(),
            &[ValuationComplete {
                company_id: "ACME".to_string(),
                npv: 12.5
            }]
        );
    }

    #[test]
    fn events_are_routed_by_kind() {
        let bus = EventBus::new();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        bus.subscribe("a.kind", move |_| {
            h.fetch_add(1, Ordering::SeqCst);
        });
        bus.emit(Event::new("a.kind"));
        bus.emit(Event::new("other.kind"));
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "only matching kind delivered"
        );
    }

    #[test]
    fn an_event_with_no_subscribers_is_logged_not_dropped() {
        let bus = EventBus::new();
        let notified = bus.emit(Event::with_payload("orphan.kind", json!({"x": 1})));
        assert_eq!(notified, 0);
        // ...but it is retained in the replayable log.
        let history = bus.history();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].kind, "orphan.kind");
    }

    #[test]
    fn history_is_replayable_for_late_subscribers() {
        let bus = EventBus::new();
        bus.emit(Event::new("valuation.completed"));
        bus.emit(Event::new("reporting.completed"));
        bus.emit(Event::new("valuation.completed"));

        let replayed = Arc::new(AtomicUsize::new(0));
        let r = replayed.clone();
        bus.replay(Some("valuation.completed"), move |_| {
            r.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(replayed.load(Ordering::SeqCst), 2);
        assert_eq!(bus.history().len(), 3);
    }

    #[test]
    fn unsubscribe_stops_delivery() {
        let bus = EventBus::new();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let id = bus.subscribe("k", move |_| {
            h.fetch_add(1, Ordering::SeqCst);
        });
        bus.emit(Event::new("k"));
        bus.unsubscribe(id);
        bus.emit(Event::new("k"));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
