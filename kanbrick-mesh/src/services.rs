//! Host-side services backing the mesh's graph-query and event imports.
//!
//! The wasmtime host imports `kbk_query_graph` (#24) and `kbk_emit_event`
//! (#27/#46) do **not** talk to a store or an event bus directly — they go
//! through [`HostServices`]. In-process this is [`LocalHostServices`]: the firm
//! graph behind a clearance-enforcing [`GuardedStore`] plus an [`EventBus`]. The
//! control-plane / executor split (#68–#71) supplies a *remote* implementation
//! that proxies these calls back to the control plane, and the runtime is
//! indifferent to which one is bound.
//!
//! Each method carries the caller's host-authoritative [`FirmContext`] and an
//! optional per-invocation capability `cap`. The capability is the bearer token
//! the executor relays to the control plane so identity stays host-authoritative
//! across the network hop (#69/#70); it is **unused in-process** and threaded
//! here only so the remote backend drops in without reshaping the trait.

use std::sync::Arc;

use kanbrick_auth::GuardedStore;
use kanbrick_core::abi::{Event, GraphQuery, GraphRows};
use kanbrick_core::FirmContext;
use kanbrick_store::Store as GraphStore;
use thiserror::Error;

use crate::event::EventBus;

/// A failure servicing a guest's host call. Surfaces to the guest as a trap.
#[derive(Debug, Error)]
pub enum HostServicesError {
    /// A `kbk_query_graph` call arrived but no graph is bound to the runtime.
    ///
    /// The message is identical to the runtime's prior in-line trap text so the
    /// guest-visible behaviour is unchanged.
    #[error("no graph bound to this runtime")]
    NoGraphBound,
    /// The graph query failed: a clearance denial, bad Cypher, or a store error.
    #[error("graph query failed: {0}")]
    Query(String),
    /// Publishing the emitted event failed.
    #[error("event emit failed: {0}")]
    Emit(String),
}

/// The host services a running guest can reach: a clearance-enforced graph query
/// and event emission, each on behalf of the caller's host-authoritative
/// [`FirmContext`].
///
/// Object-safe so the runtime can hold an `Arc<dyn HostServices>` and swap the
/// in-process [`LocalHostServices`] for a remote backend without touching the
/// dispatch path.
pub trait HostServices: Send + Sync {
    /// Run `query` on behalf of `ctx`, returning clearance-filtered rows. `cap`
    /// is the per-invocation capability (ignored in-process; used by the remote
    /// backend to authorize the call to the control plane).
    fn query_graph(
        &self,
        ctx: &FirmContext,
        cap: Option<&str>,
        query: &GraphQuery,
    ) -> Result<GraphRows, HostServicesError>;

    /// Publish `event` emitted by a guest running on behalf of `ctx`. `cap` is
    /// the per-invocation capability (ignored in-process; used by the remote
    /// backend).
    fn emit_event(
        &self,
        ctx: &FirmContext,
        cap: Option<&str>,
        event: &Event,
    ) -> Result<(), HostServicesError>;
}

/// The in-process backing for [`HostServices`]: an optional firm graph behind a
/// clearance-enforcing [`GuardedStore`], plus an optional [`EventBus`].
///
/// This is what [`MeshRuntime::with_store`](crate::MeshRuntime::with_store) and
/// [`with_bus`](crate::MeshRuntime::with_bus) compose. With no graph bound a
/// `query_graph` call returns [`HostServicesError::NoGraphBound`] (surfacing to
/// the guest as a trap); with no bus bound an emitted event is logged and
/// dropped — matching the runtime's prior behaviour exactly.
#[derive(Clone, Default)]
pub struct LocalHostServices {
    store: Option<Arc<GraphStore>>,
    bus: Option<EventBus>,
}

impl LocalHostServices {
    /// Build the local services from an optional graph and bus.
    pub fn new(store: Option<Arc<GraphStore>>, bus: Option<EventBus>) -> Self {
        LocalHostServices { store, bus }
    }

    /// Whether any backing (graph or bus) is bound. An unbound instance behaves
    /// as "no services": `query_graph` traps and `emit_event` drops.
    pub(crate) fn is_bound(&self) -> bool {
        self.store.is_some() || self.bus.is_some()
    }

    /// Set the bound graph, preserving any bound bus. Builder helper for
    /// [`MeshRuntime::with_store`](crate::MeshRuntime::with_store) — order
    /// relative to `with_bus` does not matter.
    pub(crate) fn with_store(mut self, store: Arc<GraphStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Set the bound bus, preserving any bound graph. Builder helper for
    /// [`MeshRuntime::with_bus`](crate::MeshRuntime::with_bus).
    pub(crate) fn with_bus(mut self, bus: EventBus) -> Self {
        self.bus = Some(bus);
        self
    }
}

impl HostServices for LocalHostServices {
    fn query_graph(
        &self,
        ctx: &FirmContext,
        _cap: Option<&str>,
        query: &GraphQuery,
    ) -> Result<GraphRows, HostServicesError> {
        let store = self.store.as_ref().ok_or(HostServicesError::NoGraphBound)?;
        let guarded =
            GuardedStore::new(store, ctx).map_err(|e| HostServicesError::Query(e.to_string()))?;
        guarded
            .query_graph(query)
            .map_err(|e| HostServicesError::Query(e.to_string()))
    }

    fn emit_event(
        &self,
        _ctx: &FirmContext,
        _cap: Option<&str>,
        event: &Event,
    ) -> Result<(), HostServicesError> {
        match &self.bus {
            Some(bus) => {
                bus.emit(event.clone());
            }
            None => tracing::info!(
                target: "kanbrick_mesh::guest",
                kind = %event.kind,
                "guest emitted an event but no bus is bound (dropped)"
            ),
        }
        Ok(())
    }
}
