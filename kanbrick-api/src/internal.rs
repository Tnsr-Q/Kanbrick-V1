//! Internal, in-cluster-only RPC surface for the control-plane / executor split
//! (#69, Track E).
//!
//! These routes are what executor pods (#70) call back into the control plane:
//!
//! * `POST /internal/graph/query` — run a guest's graph read under the caller's
//!   real clearance, authorized by a per-invocation capability.
//! * `POST /internal/events` — publish a guest-emitted event onto the CP bus.
//! * `GET  /internal/assets/{sha256}` — fetch a content-addressed guest artifact.
//! * `GET  /internal/registry` — list activated guests + the registry generation.
//!
//! This router is built separately from the public [`router`](crate::router) and
//! is mounted on its own ClusterIP-only listener in split deploys (#70/#71) —
//! **never** on the public ingress. Every route requires the shared transport
//! secret (`x-kanbrick-internal-token`); the graph/event callbacks additionally
//! require a valid capability resolved against [`InvocationCaps`](crate::InvocationCaps).
//! The two graph/event handlers deliberately ignore any caller-supplied identity
//! and recover it server-side from the capability, so a compromised executor
//! cannot act above the clearance the control plane bound to the invocation.

use axum::extract::{Path, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use kanbrick_auth::GuardedStore;
use kanbrick_core::abi::{Event, GraphQuery, GraphRows};
use kanbrick_core::ClearanceLevel;
use kanbrick_store::{list_guest_policies, read_registry_generation};
use serde::{Deserialize, Serialize};

use crate::{asset_error, ApiError, AppState};

/// Header carrying the shared transport secret for the internal RPC surface.
const HEADER_INTERNAL_TOKEN: &str = "x-kanbrick-internal-token";

/// Build the internal RPC router. Mount on a dedicated, ClusterIP-only listener
/// (#70/#71); never expose it through the public ingress.
pub fn internal_router(state: AppState) -> Router {
    Router::new()
        .route("/internal/graph/query", post(graph_query))
        .route("/internal/events", post(emit_event))
        .route("/internal/assets/{sha256}", get(fetch_asset))
        .route("/internal/registry", get(registry))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_internal_token,
        ))
        .with_state(state)
}

/// Reject any request to the internal surface that does not present the shared
/// transport secret. Fails closed: if no token is configured, all requests are
/// denied. The comparison is constant-time to avoid a timing oracle.
async fn require_internal_token(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let provided = req
        .headers()
        .get(HEADER_INTERNAL_TOKEN)
        .and_then(|v| v.to_str().ok());
    let ok = match (state.internal_token.as_deref(), provided) {
        (Some(expected), Some(got)) => ct_eq(expected.as_bytes(), got.as_bytes()),
        _ => false,
    };
    if ok {
        Ok(next.run(req).await)
    } else {
        Err(ApiError::unauthorized("missing or invalid internal token"))
    }
}

/// Constant-time byte-slice equality (defense-in-depth for the transport secret).
/// The length is allowed to leak; the secret's content is not.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ── /internal/graph/query ───────────────────────────────────────────────────

/// `POST /internal/graph/query` body.
#[derive(Debug, Deserialize)]
struct GraphQueryRequest {
    /// The per-invocation capability minted by the control plane.
    cap: String,
    /// The query to run under the capability's bound clearance.
    query: GraphQuery,
}

/// Run a guest's graph read on behalf of the capability's bound identity, through
/// the clearance-enforcing [`GuardedStore`]. An unknown/expired capability is a
/// `401`; a clearance denial surfaces as the usual `403`.
async fn graph_query(
    State(state): State<AppState>,
    Json(req): Json<GraphQueryRequest>,
) -> Result<Json<GraphRows>, ApiError> {
    let ctx = state
        .caps
        .resolve(&req.cap)
        .ok_or_else(|| ApiError::unauthorized("invalid or expired capability"))?;
    let guarded = GuardedStore::new(&state.store, &ctx)?;
    let rows = guarded.query_graph(&req.query)?;
    Ok(Json(rows))
}

// ── /internal/events ────────────────────────────────────────────────────────

/// `POST /internal/events` body.
#[derive(Debug, Deserialize)]
struct EmitEventRequest {
    /// The per-invocation capability minted by the control plane.
    cap: String,
    /// The event the guest emitted.
    event: Event,
}

/// Publish a guest-emitted event onto the control-plane bus. The capability is
/// validated (only an authorized invocation may publish) even though the bus
/// publish itself is identity-agnostic.
async fn emit_event(
    State(state): State<AppState>,
    Json(req): Json<EmitEventRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .caps
        .resolve(&req.cap)
        .ok_or_else(|| ApiError::unauthorized("invalid or expired capability"))?;
    state.bus.emit(req.event);
    Ok(StatusCode::NO_CONTENT)
}

// ── /internal/assets/{sha256} ───────────────────────────────────────────────

/// Fetch a content-addressed guest artifact by its hex SHA-256. The digest is
/// re-verified by the asset store on read; a miss is `404`, a malformed digest
/// `400`, and an integrity failure `500`.
async fn fetch_asset(
    State(state): State<AppState>,
    Path(sha256): Path<String>,
) -> Result<Response, ApiError> {
    let uri = format!("tachyon://sha256:{}", sha256.to_ascii_lowercase());
    let bytes = state.assets.get(&uri).map_err(asset_error)?;
    Ok(([(header::CONTENT_TYPE, "application/wasm")], bytes).into_response())
}

// ── /internal/registry ──────────────────────────────────────────────────────

/// One guest in the registry listing.
#[derive(Debug, Serialize)]
struct RegistryGuest {
    name: String,
    version: String,
    min_clearance: ClearanceLevel,
    asset_uri: String,
    source: String,
}

/// `GET /internal/registry` response.
#[derive(Debug, Serialize)]
struct RegistryResponse {
    /// Monotonic counter bumped on every activation; executors reconcile on bump.
    generation: u64,
    /// Every persisted guest policy (embedded + registry).
    guests: Vec<RegistryGuest>,
}

/// List the activated-guest set and the current registry generation so an
/// executor can replay (on boot) and reconcile (on a generation bump).
async fn registry(State(state): State<AppState>) -> Result<Json<RegistryResponse>, ApiError> {
    let generation = read_registry_generation(&state.store)?;
    let guests = list_guest_policies(&state.store)?
        .into_iter()
        .map(|p| RegistryGuest {
            name: p.guest_name,
            version: p.version,
            min_clearance: p.min_clearance,
            asset_uri: p.asset_uri,
            source: p.source,
        })
        .collect();
    Ok(Json(RegistryResponse { generation, guests }))
}
