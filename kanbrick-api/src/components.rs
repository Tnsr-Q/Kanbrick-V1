//! `/me/components` — the visualizer's read surface (P10.4, #116).
//!
//! Enumerates every registered component: the WASM guests in the
//! [`MeshRuntime`](kanbrick_mesh::MeshRuntime) registry — joined with their live
//! invocation counters ([`GuestMetric`], the same source as `/metrics`) and their
//! clearance floor (the persisted `GuestPolicy`) — plus any sidecar/plugin that
//! self-registered over the internal RPC surface (P10.6, #118). Built entirely from
//! existing sources — no new metrics fabric.
//!
//! Clearance-gated and audited; identity is host-authoritative (ADR-0002/0016) via
//! the [`AuthedContext`] extractor. The response shape mirrors 1:1 to the cockpit's
//! TS `ComponentStatus` for the P10.5 visualizer UI.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use axum::extract::State;
use axum::Json;
use kanbrick_auth::{require_clearance, AuditLog};
use kanbrick_core::ClearanceLevel;
use kanbrick_mesh::GuestMetric;
use kanbrick_store::list_guest_policies;
use serde::{Deserialize, Serialize};

use crate::{ApiError, AppState, AuthedContext};

/// Minimum clearance to read the component visualizer.
///
/// The component catalogue and its failure/health counters are operational system
/// internals — the same data the in-cluster-only `/metrics` surface treats as
/// sensitive (it reveals the guest catalogue). It is therefore gated at strategic
/// (L4) clearance, aligned with the `/admin` bar and below the L5 mutate operations
/// (asset upload / guest activation). Adjust this single constant to change the bar.
const COMPONENTS_CLEARANCE: ClearanceLevel = ClearanceLevel::L4;

/// One running component's status.
///
/// A flat serde struct mirrored 1:1 to the cockpit TS `ComponentStatus`
/// (snake_case fields; `clearance` is the serialized [`ClearanceLevel`],
/// `"L1"`..`"L5"`).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ComponentStatus {
    /// Registered component name.
    name: String,
    /// Self-reported version.
    version: String,
    /// Invocations currently executing (a gauge).
    active: i64,
    /// Invocations that returned a response.
    completed: u64,
    /// Invocations that failed (trap, bad output, resource limit, …).
    failed: u64,
    /// Invocations killed for exceeding their wall-clock budget.
    timed_out: u64,
    /// Minimum clearance required to invoke the component.
    clearance: ClearanceLevel,
}

// ── Self-registered components (P10.6, #118) ─────────────────────────────────

/// A component descriptor a sidecar/plugin self-registers over the internal RPC
/// surface (`POST /internal/components/register`). It is both the wire body of that
/// endpoint and the value stored in the [`ComponentRegistry`]. Unlike a WASM guest,
/// a self-registered component reports no invocation counters — only its identity,
/// version, and the clearance floor required to drive it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredComponent {
    /// Unique component name (the registry key; re-registering replaces it).
    pub name: String,
    /// Self-reported version.
    pub version: String,
    /// Minimum clearance required to invoke the component.
    pub clearance: ClearanceLevel,
}

/// In-process registry of self-registered sidecar/plugin components, folded into
/// [`list_components`] alongside the WASM guests. Cheaply cloneable (it shares one
/// lock-guarded map) and concurrency-safe — registrations arrive on the internal
/// RPC surface while `/me/components` reads it. Last write wins per name, so a
/// component refreshing its descriptor never duplicates.
#[derive(Clone, Default)]
pub struct ComponentRegistry {
    inner: Arc<RwLock<HashMap<String, RegisteredComponent>>>,
}

impl ComponentRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        ComponentRegistry::default()
    }

    /// Record (or refresh) a component, keyed by name. Idempotent: re-registering
    /// the same name replaces the prior descriptor.
    pub fn register(&self, component: RegisteredComponent) {
        self.inner
            .write()
            .expect("component registry lock")
            .insert(component.name.clone(), component);
    }

    /// A snapshot of every registered component (unordered; the caller sorts).
    fn snapshot(&self) -> Vec<RegisteredComponent> {
        self.inner
            .read()
            .expect("component registry lock")
            .values()
            .cloned()
            .collect()
    }

    /// Number of registered components. Test/diagnostic helper (named like
    /// `InvocationCaps::live_count` to avoid clippy's `len_without_is_empty` on a
    /// `pub` type when clippy runs over test targets).
    #[cfg(test)]
    pub fn live_count(&self) -> usize {
        self.inner.read().expect("component registry lock").len()
    }
}

/// `GET /me/components` — the live component catalogue with health counters.
///
/// Joins the runtime registry (name + version), the per-guest invocation metrics
/// ([`GuestMetric`]), and the persisted clearance floor (`GuestPolicy`) by name.
/// The registry is the authoritative set of running components and is already
/// sorted by name, so the output is deterministic. Clearance-gated and audited.
pub(crate) async fn list_components(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
) -> Result<Json<Vec<ComponentStatus>>, ApiError> {
    require_clearance(&ctx, COMPONENTS_CLEARANCE)?;

    // Live per-guest counters, keyed by name (the same snapshot `/metrics` renders).
    let metrics: HashMap<String, GuestMetric> = state
        .mesh
        .metrics_snapshot()
        .into_iter()
        .map(|m| (m.name.clone(), m))
        .collect();
    // Each component's clearance floor from its persisted policy, in one read.
    let clearances: HashMap<String, ClearanceLevel> = list_guest_policies(&state.store)?
        .into_iter()
        .map(|p| (p.guest_name, p.min_clearance))
        .collect();

    let mut components: Vec<ComponentStatus> = state
        .mesh
        .guests()
        .into_iter()
        .map(|g| {
            // Registered guests always have a metrics entry; default to zero defensively.
            let (active, completed, failed, timed_out) = match metrics.get(&g.name) {
                Some(m) => (m.active, m.completed, m.failed, m.timed_out),
                None => (0, 0, 0, 0),
            };
            // Registered guests always have a seeded policy; L1 is a defensive floor.
            let clearance = clearances
                .get(&g.name)
                .copied()
                .unwrap_or(ClearanceLevel::L1);
            ComponentStatus {
                name: g.name,
                version: g.version,
                active,
                completed,
                failed,
                timed_out,
                clearance,
            }
        })
        .collect();

    // Fold in self-registered sidecars/plugins (#118). A live WASM guest is
    // authoritative over a self-registered claim to the same name, so a collision
    // keeps the guest. Self-registered components report no invocation counters.
    let guest_names: HashSet<String> = components.iter().map(|c| c.name.clone()).collect();
    for reg in state.components.snapshot() {
        if guest_names.contains(&reg.name) {
            continue;
        }
        components.push(ComponentStatus {
            name: reg.name,
            version: reg.version,
            active: 0,
            completed: 0,
            failed: 0,
            timed_out: 0,
            clearance: reg.clearance,
        });
    }
    // Stable, name-sorted output across both component sources.
    components.sort_by(|a, b| a.name.cmp(&b.name));

    AuditLog::new(&state.store).record(&ctx, "components:list")?;
    Ok(Json(components))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(name: &str, version: &str) -> RegisteredComponent {
        RegisteredComponent {
            name: name.to_string(),
            version: version.to_string(),
            clearance: ClearanceLevel::L3,
        }
    }

    #[test]
    fn register_then_snapshot_round_trips() {
        let reg = ComponentRegistry::new();
        assert_eq!(reg.live_count(), 0);
        reg.register(descriptor("ledger-sync", "1.0.0"));
        reg.register(descriptor("billing", "0.3.0"));
        assert_eq!(reg.live_count(), 2);
        let mut names: Vec<String> = reg.snapshot().into_iter().map(|c| c.name).collect();
        names.sort();
        assert_eq!(names, ["billing", "ledger-sync"]);
    }

    #[test]
    fn re_registration_is_last_write_wins() {
        let reg = ComponentRegistry::new();
        reg.register(descriptor("ledger-sync", "1.0.0"));
        reg.register(descriptor("ledger-sync", "2.0.0"));
        assert_eq!(reg.live_count(), 1, "same name does not duplicate");
        assert_eq!(reg.snapshot()[0].version, "2.0.0", "latest descriptor wins");
    }

    #[test]
    fn registry_clones_share_one_map() {
        // Cheap-clone semantics: a clone observes registrations on the original
        // (both share the Arc), matching how AppState clones share state.
        let reg = ComponentRegistry::new();
        let clone = reg.clone();
        reg.register(descriptor("ledger-sync", "1.0.0"));
        assert_eq!(clone.live_count(), 1, "a clone shares the same backing map");
    }
}
