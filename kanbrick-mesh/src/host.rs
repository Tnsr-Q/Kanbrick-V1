//! The host's implementation of the guest-facing capability surface (#23).
//!
//! [`MeshHost`] implements [`kanbrick_core::abi::HostFunctions`] for one
//! invocation, holding the **host-authoritative** [`FirmContext`]. It is the
//! canonical place the four capabilities are serviced:
//!
//! * `get_firm_context` / `log` are fully live here.
//! * `emit_event` buffers events until the real pub/sub bus lands (#27).
//! * `query_graph` is wired to the clearance-enforcing `GuardedStore` in #24;
//!   until then it returns a clear "not yet wired" error.
//!
//! The WASM-facing side of context propagation (the `kbk_ctx_*` imports) lives in
//! [`crate::runtime`]; both read the same host-supplied identity, which a guest
//! can never set or forge.

use std::sync::Mutex;

use kanbrick_core::abi::{Event, GraphQuery, GraphRows, HostFunctions, LogLevel};
use kanbrick_core::{Error, FirmContext, Result};

/// Per-invocation host state servicing a guest's [`HostFunctions`] calls.
pub struct MeshHost {
    ctx: FirmContext,
    events: Mutex<Vec<Event>>,
}

impl MeshHost {
    /// Bind the host to the caller's `ctx` for one invocation.
    pub fn new(ctx: FirmContext) -> Self {
        MeshHost {
            ctx,
            events: Mutex::new(Vec::new()),
        }
    }

    /// Take the events emitted so far (buffered until the #27 event bus lands).
    pub fn drain_events(&self) -> Vec<Event> {
        std::mem::take(&mut self.events.lock().expect("event buffer lock"))
    }
}

impl HostFunctions for MeshHost {
    fn get_firm_context(&self) -> FirmContext {
        self.ctx.clone()
    }

    fn query_graph(&self, _query: GraphQuery) -> Result<GraphRows> {
        Err(Error::Internal(
            "query_graph is wired to GuardedStore in #24".to_string(),
        ))
    }

    fn emit_event(&self, event: Event) -> Result<()> {
        self.events.lock().expect("event buffer lock").push(event);
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
    fn query_graph_reports_it_is_not_yet_wired() {
        let h = host(ClearanceLevel::L5);
        let err = h
            .query_graph(GraphQuery::new("MATCH (n) RETURN n"))
            .unwrap_err();
        assert_eq!(err.kind(), kanbrick_core::ErrorKind::Internal);
        assert!(err.to_string().contains("#24"));
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
