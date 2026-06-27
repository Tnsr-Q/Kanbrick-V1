//! `/me/components` — the visualizer's read surface (P10.4, #116).
//!
//! Enumerates every registered component (today: the WASM guests in the
//! [`MeshRuntime`](kanbrick_mesh::MeshRuntime) registry) joined with its live
//! invocation counters ([`GuestMetric`], the same source as `/metrics`) and its
//! clearance floor (the persisted `GuestPolicy`). Built entirely from existing
//! sources — no new metrics fabric.
//!
//! Clearance-gated and audited; identity is host-authoritative (ADR-0002/0016) via
//! the [`AuthedContext`] extractor. The response shape mirrors 1:1 to the cockpit's
//! TS `ComponentStatus` for the P10.5 visualizer UI.

use std::collections::HashMap;

use axum::extract::State;
use axum::Json;
use kanbrick_auth::{require_clearance, AuditLog};
use kanbrick_core::ClearanceLevel;
use kanbrick_mesh::GuestMetric;
use kanbrick_store::list_guest_policies;
use serde::Serialize;

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

    let components: Vec<ComponentStatus> = state
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

    AuditLog::new(&state.store).record(&ctx, "components:list")?;
    Ok(Json(components))
}
