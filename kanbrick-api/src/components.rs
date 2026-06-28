//! `/me/components` — the visualizer's read surface (P10.4, #116).
//!
//! Enumerates every registered component across all three kinds (Requirement 2.1,
//! P10.7, #119):
//!
//! * **guests** — the WASM guests in the [`MeshRuntime`](kanbrick_mesh::MeshRuntime)
//!   registry, joined with their live invocation counters ([`GuestMetric`], the same
//!   source as `/metrics`) and clearance floor (the persisted `GuestPolicy`);
//! * **sidecars** — any sidecar/plugin that self-registered over the internal RPC
//!   surface (P10.6, #118);
//! * **services** — the in-process firm-OS services backing `AppState` (the graph
//!   store, event bus, asset store, identity, capability registry, provider-key
//!   custody, and — when the control-plane/executor split is configured — the
//!   executor forwarder and internal-RPC surface).
//!
//! Each carries a [`ComponentKind`] discriminator so the visualizer can render all
//! three uniformly. Built entirely from existing sources — no new metrics fabric.
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

/// Which kind of component a catalogue row describes (P10.7, #119). Serializes
/// snake_case (`"guest"` | `"sidecar"` | `"service"`), mirrored 1:1 by the cockpit
/// TS `ComponentKind` union so the visualizer can render all three uniformly.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ComponentKind {
    /// A WASM business guest in the mesh registry (carries live invocation metrics).
    Guest,
    /// A sidecar/plugin that self-registered over the internal RPC surface (#118).
    Sidecar,
    /// An in-process firm-OS service backing `AppState` (#119).
    Service,
}

/// One running component's status.
///
/// A flat serde struct mirrored 1:1 to the cockpit TS `ComponentStatus`
/// (snake_case fields; `clearance` is the serialized [`ClearanceLevel`],
/// `"L1"`..`"L5"`). Only guests carry live invocation counters; sidecars and
/// services report zeroes (they have no `GuestMetric`), and the [`kind`](Self::kind)
/// discriminator tells them apart.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ComponentStatus {
    /// Registered component name.
    name: String,
    /// Self-reported version.
    version: String,
    /// Invocations currently executing (a gauge). Always `0` for non-guests.
    active: i64,
    /// Invocations that returned a response. Always `0` for non-guests.
    completed: u64,
    /// Invocations that failed (trap, bad output, resource limit, …). `0` for non-guests.
    failed: u64,
    /// Invocations killed for exceeding their wall-clock budget. `0` for non-guests.
    timed_out: u64,
    /// Minimum clearance required to invoke (or manage) the component.
    clearance: ClearanceLevel,
    /// Which kind of component this row is (guest / sidecar / service).
    kind: ComponentKind,
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

// ── In-process services (P10.7, #119) ────────────────────────────────────────

/// Version reported for the in-process services — the API crate version, since
/// they all ship in this binary.
const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// One in-process firm-OS service surfaced in the visualizer: a stable name and the
/// clearance floor to manage it. The `version` is the crate version and the `kind`
/// is always [`ComponentKind::Service`]; an "active" service is one present in this
/// list, so there are no invocation counters to report.
struct ServiceSpec {
    /// Stable service name shown in the catalogue.
    name: &'static str,
    /// Minimum clearance to manage the service.
    clearance: ClearanceLevel,
}

/// The always-present in-process services backing `AppState`. These handles exist on
/// every node regardless of deployment shape. Sensitive data planes (the firm graph,
/// the capability registry) sit at L5; the rest at the L4 visualizer bar.
const CORE_SERVICES: &[ServiceSpec] = &[
    ServiceSpec {
        name: "graph-store",
        clearance: ClearanceLevel::L5,
    },
    ServiceSpec {
        name: "identity",
        clearance: ClearanceLevel::L4,
    },
    ServiceSpec {
        name: "event-bus",
        clearance: ClearanceLevel::L4,
    },
    ServiceSpec {
        name: "asset-store",
        clearance: ClearanceLevel::L4,
    },
    ServiceSpec {
        name: "capability-registry",
        clearance: ClearanceLevel::L5,
    },
    ServiceSpec {
        name: "provider-keys",
        clearance: ClearanceLevel::L4,
    },
];

/// Build a [`ComponentStatus`] row for an in-process service (no invocation
/// counters; `kind = Service`).
fn service_status(name: &str, clearance: ClearanceLevel) -> ComponentStatus {
    ComponentStatus {
        name: name.to_string(),
        version: SERVICE_VERSION.to_string(),
        active: 0,
        completed: 0,
        failed: 0,
        timed_out: 0,
        clearance,
        kind: ComponentKind::Service,
    }
}

/// Enumerate the in-process services from the live `AppState`. The core services are
/// always present; the executor forwarder and internal-RPC surface appear only when
/// the control-plane/executor split is configured (so the set reflects the live
/// configuration, #119).
fn service_components(state: &AppState) -> Vec<ComponentStatus> {
    let mut services: Vec<ComponentStatus> = CORE_SERVICES
        .iter()
        .map(|s| service_status(s.name, s.clearance))
        .collect();
    // Conditional services reflect the live AppState wiring. Their presence reveals
    // one bit of deployment shape (control-plane/executor split vs monolith) to the
    // L4+ caller — names only, never the executor URL or transport secret — an
    // acceptable disclosure to a trusted Strategic-tier insider on a host-authoritative
    // OS (the guest catalogue at this same bar is more sensitive).
    if state.executor.is_some() {
        services.push(service_status("executor-forwarder", ClearanceLevel::L4));
    }
    if state.internal_token.is_some() {
        services.push(service_status("internal-rpc", ClearanceLevel::L5));
    }
    services
}

/// `GET /me/components` — the live component catalogue with health counters.
///
/// Unions three kinds (#119): WASM guests (name + version + live [`GuestMetric`]
/// counters + persisted clearance floor), self-registered sidecars (#118), and the
/// in-process firm-OS services. Each row carries a [`ComponentKind`]. Names are
/// deduplicated across sources in authority order (guest > sidecar > service) and the
/// output is name-sorted, so it is deterministic. Clearance-gated and audited.
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
                kind: ComponentKind::Guest,
            }
        })
        .collect();

    // Dedup across all three sources in authority order: a live WASM guest wins a
    // name collision with a self-registered claim, and both win over an in-process
    // service. `seen.insert` returns false when the name is already taken.
    let mut seen: HashSet<String> = components.iter().map(|c| c.name.clone()).collect();

    // Fold in self-registered sidecars/plugins (#118) — no invocation counters.
    for reg in state.components.snapshot() {
        if !seen.insert(reg.name.clone()) {
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
            kind: ComponentKind::Sidecar,
        });
    }

    // Fold in the in-process firm-OS services (#119).
    for svc in service_components(&state) {
        if !seen.insert(svc.name.clone()) {
            continue;
        }
        components.push(svc);
    }

    // Stable, name-sorted output across all three component sources.
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

    #[test]
    fn component_kind_serializes_snake_case() {
        assert_eq!(serde_json::to_value(ComponentKind::Guest).unwrap(), "guest");
        assert_eq!(
            serde_json::to_value(ComponentKind::Sidecar).unwrap(),
            "sidecar"
        );
        assert_eq!(
            serde_json::to_value(ComponentKind::Service).unwrap(),
            "service"
        );
    }

    #[test]
    fn service_status_has_service_kind_and_no_counters() {
        let v = serde_json::to_value(service_status("graph-store", ClearanceLevel::L5)).unwrap();
        assert_eq!(v["kind"], "service");
        assert_eq!(v["name"], "graph-store");
        assert_eq!(v["clearance"], "L5");
        assert_eq!(v["version"], SERVICE_VERSION);
        for counter in ["active", "completed", "failed", "timed_out"] {
            assert_eq!(v[counter], 0, "a service carries no invocation counters");
        }
    }

    #[test]
    fn core_services_are_uniquely_named() {
        let mut names: Vec<&str> = CORE_SERVICES.iter().map(|s| s.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "core service names must be unique");
        assert!(count >= 1, "there is at least one core service");
    }
}
