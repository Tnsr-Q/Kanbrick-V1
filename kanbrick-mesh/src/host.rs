//! The host's implementation of the guest-facing capability surface (#23).
//!
//! [`MeshHost`] implements [`kanbrick_core::abi::HostFunctions`] for one
//! invocation, holding the **host-authoritative** [`FirmContext`]. It is the
//! canonical place the four capabilities are serviced:
//!
//! * `get_firm_context` / `log` are fully live here.
//! * `emit_event` buffers events until the real pub/sub bus lands (#27).
//! * `query_graph` routes through the clearance-enforcing
//!   [`GuardedStore`](kanbrick_auth::GuardedStore) (#24) when a store is bound;
//!   without one it returns a clear error.
//!
//! The WASM-facing side of context propagation (the `kbk_ctx_*` imports) lives in
//! [`crate::runtime`]; both read the same host-supplied identity, which a guest
//! can never set or forge.

use std::sync::{Arc, Mutex};

use kanbrick_auth::GuardedStore;
use kanbrick_core::abi::{Event, GraphQuery, GraphRows, HostFunctions, LogLevel};
use kanbrick_core::{Error, FirmContext, Result};
use kanbrick_store::Store;

use crate::event::EventBus;

/// Per-invocation host state servicing a guest's [`HostFunctions`] calls.
pub struct MeshHost {
    ctx: FirmContext,
    store: Option<Arc<Store>>,
    bus: Option<EventBus>,
    events: Mutex<Vec<Event>>,
}

impl MeshHost {
    /// Bind the host to the caller's `ctx`. With no store, `query_graph` errors;
    /// with no bus, `emit_event` buffers into [`drain_events`](Self::drain_events).
    /// Attach a graph and a bus with [`with_store`](Self::with_store) /
    /// [`with_bus`](Self::with_bus).
    pub fn new(ctx: FirmContext) -> Self {
        MeshHost {
            ctx,
            store: None,
            bus: None,
            events: Mutex::new(Vec::new()),
        }
    }

    /// Bind the firm graph, so `query_graph` runs through the clearance-enforcing
    /// [`GuardedStore`] (#24). Builder-style.
    pub fn with_store(mut self, store: Arc<Store>) -> Self {
        self.store = Some(store);
        self
    }

    /// Bind an [`EventBus`], so `emit_event` publishes onto it (#27). Builder-style.
    pub fn with_bus(mut self, bus: EventBus) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Take the events buffered by `emit_event` when no bus is attached.
    pub fn drain_events(&self) -> Vec<Event> {
        std::mem::take(&mut self.events.lock().expect("event buffer lock"))
    }
}

impl HostFunctions for MeshHost {
    fn get_firm_context(&self) -> FirmContext {
        self.ctx.clone()
    }

    fn query_graph(&self, query: GraphQuery) -> Result<GraphRows> {
        let store = self.store.as_ref().ok_or_else(|| {
            Error::Internal("query_graph: no store is bound to this host".to_string())
        })?;
        // Every guest query runs under the caller's host-authoritative context
        // through the audited, clearance-filtering interceptor (#18/#24).
        let guarded = GuardedStore::new(store, &self.ctx)?;
        guarded.query_graph(&query)
    }

    fn emit_event(&self, event: Event) -> Result<()> {
        match &self.bus {
            Some(bus) => {
                bus.emit(event);
            }
            None => self.events.lock().expect("event buffer lock").push(event),
        }
        Ok(())
    }

    fn log(&self, level: LogLevel, message: &str) {
        match level {
            LogLevel::Error => tracing::error!(target: "kanbrick_mesh::guest", "{message}"),
            LogLevel::Warn => tracing::warn!(target: "kanbrick_mesh::guest", "{message}"),
            LogLevel::Info => tracing::info!(target: "kanbrick_mesh::guest", "{message}"),
            LogLevel::Debug => tracing::debug!(target: "kanbrick_mesh::guest", "{message}"),
            LogLevel::Trace => tracing::trace!(target: "kanbrick_mesh::guest", "{message}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanbrick_core::ClearanceLevel;
    use serde_json::json;
    use uuid::Uuid;

    fn host(clearance: ClearanceLevel) -> MeshHost {
        MeshHost::new(FirmContext::new(Uuid::nil(), "u@kanbrick.com", clearance))
    }

    #[test]
    fn get_firm_context_returns_the_injected_identity() {
        let h = host(ClearanceLevel::L4);
        assert_eq!(h.get_firm_context().clearance, ClearanceLevel::L4);
        assert_eq!(h.get_firm_context().email, "u@kanbrick.com");
    }

    #[test]
    fn emit_event_buffers_until_drained() {
        let h = host(ClearanceLevel::L1);
        h.emit_event(Event::with_payload("x.done", json!({"n": 1})))
            .unwrap();
        h.emit_event(Event::new("y.done")).unwrap();
        let drained = h.drain_events();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].kind, "x.done");
        // Draining empties the buffer.
        assert!(h.drain_events().is_empty());
    }

    #[test]
    fn emit_event_publishes_to_an_attached_bus() {
        use crate::event::EventBus;
        let bus = EventBus::new();
        let h = MeshHost::new(FirmContext::new(
            Uuid::nil(),
            "u@kanbrick.com",
            ClearanceLevel::L3,
        ))
        .with_bus(bus.clone());

        h.emit_event(Event::new("valuation.completed")).unwrap();
        // It went to the bus (the log), not the local buffer.
        assert!(h.drain_events().is_empty());
        assert_eq!(bus.history().len(), 1);
        assert_eq!(bus.history()[0].kind, "valuation.completed");
    }

    #[test]
    fn query_graph_without_a_store_errors() {
        let h = host(ClearanceLevel::L5);
        let err = h
            .query_graph(GraphQuery::new("MATCH (n) RETURN n"))
            .unwrap_err();
        assert_eq!(err.kind(), kanbrick_core::ErrorKind::Internal);
        assert!(err.to_string().contains("no store"));
    }

    fn seeded_store() -> (tempfile::TempDir, std::sync::Arc<kanbrick_store::Store>) {
        let dir = tempfile::tempdir().unwrap();
        let store = kanbrick_store::Store::open(dir.path()).unwrap();
        let seed = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../seed/kanbrick_seed_data.cypher"
        ))
        .unwrap();
        kanbrick_store::Migrator::firm(seed).run(&store).unwrap();
        (dir, std::sync::Arc::new(store))
    }

    const ALL_COMPANIES: &str = "MATCH (c:Company) RETURN c.company_id, c.name";

    #[test]
    fn query_graph_routes_through_guardedstore_and_filters_by_clearance() {
        let (_d, store) = seeded_store();

        // An L3 lead's guest query comes back scoped to their 5 segment companies.
        let lead = FirmContext::new(
            Uuid::new_v4(),
            "tyler.begemann@kanbrick.com",
            ClearanceLevel::L3,
        );
        let host3 = MeshHost::new(lead).with_store(store.clone());
        let rows = host3.query_graph(GraphQuery::new(ALL_COMPANIES)).unwrap();
        assert_eq!(rows.len(), 5);

        // The CEO (L5) sees all 9 through the same host call.
        let ceo = FirmContext::new(
            Uuid::new_v4(),
            "tracy.brittcool@kanbrick.com",
            ClearanceLevel::L5,
        );
        let host5 = MeshHost::new(ceo).with_store(store);
        let rows = host5.query_graph(GraphQuery::new(ALL_COMPANIES)).unwrap();
        assert_eq!(rows.len(), 9);
    }

    #[test]
    fn log_does_not_panic_at_any_level() {
        let h = host(ClearanceLevel::L2);
        for level in [
            LogLevel::Error,
            LogLevel::Warn,
            LogLevel::Info,
            LogLevel::Debug,
            LogLevel::Trace,
        ] {
            h.log(level, "guest log line");
        }
    }
}
