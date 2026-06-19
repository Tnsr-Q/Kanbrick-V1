//! # kanbrick-api
//!
//! HTTP surface for Kanbrick-V1 — the canonical integration surface that wires
//! auth, the WASM mesh, and the graph into one path: **HTTP → Auth → Mesh →
//! Guest → Graph** (the PRD's stated architecture; Phase 6 #47).
//!
//! * `POST /login` — email + password → JWT (issue #15).
//! * `GET  /me` — returns the caller's identity; requires a valid JWT.
//! * `GET  /admin` — a clearance-gated route requiring L4+ (issue #16).
//! * `GET  /health` — liveness + embedded-guest count (#51).
//! * `GET  /metrics` — unauthenticated Prometheus mesh-pressure metrics (#63);
//!   in-cluster scrape surface only.
//! * `POST /guests/{name}` — authenticate, gate by clearance, **audit**, and
//!   invoke a WASM guest under the caller's host-authoritative `FirmContext`,
//!   returning its response (#47).
//!
//! The three business guests are **embedded** in the binary at build time
//! (`include_bytes!`), so a single self-contained binary serves them (#53). The
//! caller's identity is supplied to a guest *only* by the host-authoritative mesh
//! (`kbk_ctx_*`); it is never taken from the request payload, so it cannot be
//! forged.
//!
//! A missing/invalid/expired JWT yields a structured `401`; insufficient
//! clearance yields a structured `403`.

use std::sync::Arc;

use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use kanbrick_auth::{require_clearance, AuditLog, JwtAuthenticator, LoginService};
use kanbrick_core::abi::GuestRequest;
use kanbrick_core::{ClearanceLevel, Error, ErrorKind, FirmContext};
use kanbrick_mesh::{EventBus, MeshError, MeshRuntime};
use kanbrick_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

mod admission;
mod metrics;

pub use admission::{AdmissionConfig, GuestAdmission};

/// The three business guests, embedded at build time (build.rs → `include_bytes!`).
const VALUATION_WASM: &[u8] = include_bytes!(env!("KANBRICK_VALUATION_GUEST_WASM"));
const REPORTING_WASM: &[u8] = include_bytes!(env!("KANBRICK_REPORTING_GUEST_WASM"));
const COMPLIANCE_WASM: &[u8] = include_bytes!(env!("KANBRICK_COMPLIANCE_GUEST_WASM"));

/// Guest version reported in the registry (the API crate version).
const GUEST_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Shared application state, cheaply cloneable (everything behind `Arc`).
#[derive(Clone)]
pub struct AppState {
    /// The embedded graph store.
    pub store: Arc<Store>,
    /// JWT issuer/validator.
    pub jwt: Arc<JwtAuthenticator>,
    /// The WASM mesh, pre-loaded with the embedded guests and bound to the store.
    pub mesh: Arc<MeshRuntime>,
    /// Per-guest admission control for the synchronous invocation path (#63).
    pub admission: Arc<GuestAdmission>,
}

impl AppState {
    /// Build state from a store and JWT authenticator, loading the embedded
    /// guests into a store-bound mesh runtime, with default admission limits.
    pub fn new(store: Store, jwt: JwtAuthenticator) -> Result<Self, MeshError> {
        Self::with_config(store, jwt, AdmissionConfig::default())
    }

    /// Like [`new`](Self::new) but with explicit per-guest admission limits (#63).
    pub fn with_config(
        store: Store,
        jwt: JwtAuthenticator,
        admission: AdmissionConfig,
    ) -> Result<Self, MeshError> {
        let store = Arc::new(store);
        let mesh = Arc::new(build_mesh(store.clone())?);
        let admission = Arc::new(GuestAdmission::new(
            mesh.guests().into_iter().map(|g| g.name),
            admission,
        ));
        Ok(AppState {
            store,
            jwt: Arc::new(jwt),
            mesh,
            admission,
        })
    }
}

/// Build the mesh runtime with the three embedded guests registered, bound to the
/// firm graph (so `query_graph` works) and an event bus (so guest `emit` works).
fn build_mesh(store: Arc<Store>) -> Result<MeshRuntime, MeshError> {
    let mut mesh = MeshRuntime::new()?
        .with_store(store)
        .with_bus(EventBus::new());
    mesh.register_module("valuation", GUEST_VERSION, VALUATION_WASM)?;
    mesh.register_module("reporting", GUEST_VERSION, REPORTING_WASM)?;
    mesh.register_module("compliance", GUEST_VERSION, COMPLIANCE_WASM)?;
    Ok(mesh)
}

/// The minimum clearance the API requires to invoke a guest (defense in depth —
/// each guest also enforces its own clearance internally). `None` ⇒ unknown
/// guest. Mirrors the guests' `REQUIRED_CLEARANCE`: valuation L3, compliance L4,
/// reporting is clearance-*tiered* so any authenticated caller may run it.
fn guest_min_clearance(name: &str) -> Option<ClearanceLevel> {
    match name {
        "valuation" => Some(ClearanceLevel::L3),
        "compliance" => Some(ClearanceLevel::L4),
        "reporting" => Some(ClearanceLevel::L1),
        _ => None,
    }
}

/// Assemble the application router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/login", post(login))
        .route("/me", get(me))
        .route("/admin", get(admin))
        .route("/health", get(health))
        .route("/metrics", get(metrics_handler))
        .route("/guests/{name}", post(invoke_guest))
        .with_state(state)
}

// ── Error responses ───────────────────────────────────────────────────────────

/// A structured API error rendered as JSON `{ "error": { "kind", "message" } }`.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    kind: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, kind: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            kind,
            message: message.into(),
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }
}

impl From<Error> for ApiError {
    fn from(err: Error) -> Self {
        let status = match err.kind() {
            ErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
            ErrorKind::NotFound => StatusCode::NOT_FOUND,
            ErrorKind::ValidationError => StatusCode::BAD_REQUEST,
            ErrorKind::QueryError | ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        // AccessDenied is an authorization failure: surface it as 403.
        let status = if matches!(err, Error::AccessDenied { .. }) {
            StatusCode::FORBIDDEN
        } else {
            status
        };
        let kind = match err.kind() {
            ErrorKind::Unauthorized if status == StatusCode::FORBIDDEN => "forbidden",
            ErrorKind::Unauthorized => "unauthorized",
            ErrorKind::NotFound => "not_found",
            ErrorKind::ValidationError => "invalid_request",
            ErrorKind::QueryError | ErrorKind::Internal => "internal",
        };
        ApiError::new(status, kind, err.to_string())
    }
}

/// Map a mesh failure onto an API error: an unknown guest is a `404`, anything
/// else is an internal `500`.
fn mesh_error(err: MeshError) -> ApiError {
    match err {
        MeshError::GuestNotFound(name) => ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("unknown guest {name}"),
        ),
        other => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            other.to_string(),
        ),
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({
            "error": { "kind": self.kind, "message": self.message }
        }));
        (self.status, body).into_response()
    }
}

// ── Auth extractor ────────────────────────────────────────────────────────────

/// Extractor that authenticates the request from its `Authorization: Bearer`
/// JWT and yields the caller's [`FirmContext`]. Rejects with `401` on any
/// missing/malformed/invalid token.
pub struct AuthedContext(pub FirmContext);

impl FromRequestParts<AppState> for AuthedContext {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ApiError::unauthorized("missing Authorization header"))?;
        let token = header
            .strip_prefix("Bearer ")
            .ok_or_else(|| ApiError::unauthorized("expected a Bearer token"))?;
        let ctx = state
            .jwt
            .validate(token)
            .map_err(|_| ApiError::unauthorized("invalid or expired token"))?;
        Ok(AuthedContext(ctx))
    }
}

// ── Request/response bodies ───────────────────────────────────────────────────

/// `POST /login` request body.
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    /// Login email.
    pub email: String,
    /// Plaintext password.
    pub password: String,
}

/// `POST /login` success body.
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    /// The signed JWT.
    pub token: String,
}

/// `GET /me` body — the caller's identity.
#[derive(Debug, Serialize)]
pub struct MeResponse {
    /// Caller email.
    pub email: String,
    /// Caller clearance.
    pub clearance: ClearanceLevel,
    /// Caller role tags.
    pub roles: Vec<String>,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, ApiError> {
    let svc = LoginService::new(&state.store, &state.jwt);
    let token = svc.login(&req.email, &req.password)?;
    Ok(Json(LoginResponse { token }))
}

async fn me(AuthedContext(ctx): AuthedContext) -> Json<MeResponse> {
    Json(MeResponse {
        email: ctx.email,
        clearance: ctx.clearance,
        roles: ctx.roles,
    })
}

async fn admin(AuthedContext(ctx): AuthedContext) -> Result<Json<MeResponse>, ApiError> {
    // Coarse clearance gate: this route requires strategic (L4) clearance.
    require_clearance(&ctx, ClearanceLevel::L4)?;
    Ok(Json(MeResponse {
        email: ctx.email,
        clearance: ctx.clearance,
        roles: ctx.roles,
    }))
}

/// `GET /health` — liveness probe with the embedded-guest count and version.
async fn health(State(state): State<AppState>) -> Json<JsonValue> {
    Json(serde_json::json!({
        "status": "healthy",
        "guests_loaded": state.mesh.guests().len(),
        "version": GUEST_VERSION,
    }))
}

/// `GET /metrics` — unauthenticated Prometheus exposition of mesh pressure (#63).
///
/// Emits per-guest invocation gauges/counters and `kanbrick_mesh_pressure_ratio`
/// for the KEDA scaler. The `guest="…"` labels reveal the guest catalogue, so this
/// is an **in-cluster scrape surface only** and must not be routed through the
/// public ingress — see `docs/SECURITY.md`.
async fn metrics_handler(State(state): State<AppState>) -> Response {
    let body =
        metrics::render_prometheus(&state.mesh.metrics_snapshot(), &state.admission.snapshot());
    ([(header::CONTENT_TYPE, metrics::CONTENT_TYPE)], body).into_response()
}

/// `POST /guests/{name}` — the canonical guest-invocation surface.
///
/// Pipeline: **JWT → FirmContext → clearance gate → admission → audit → guest**.
/// The caller's identity is host-authoritative (from the validated token,
/// propagated by the mesh); nothing in the request body can set or forge it. WASM
/// execution runs on a blocking thread so it never stalls the async runtime, and
/// per-guest admission control sheds load past the queue limit (`429`) under
/// pressure (#63).
async fn invoke_guest(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Path(name): Path<String>,
    Json(payload): Json<JsonValue>,
) -> Result<Json<JsonValue>, ApiError> {
    // Unknown guest → 404; insufficient clearance → 403 (the guest also enforces).
    let min = guest_min_clearance(&name).ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("unknown guest {name}"),
        )
    })?;
    require_clearance(&ctx, min)?;

    // Admission control (#63): bound per-guest concurrency and shed load past the
    // queue limit, so a burst returns 429 instead of exhausting the blocking pool.
    // The permit is held for the whole invocation (dropped at end of scope).
    let _permit = state.admission.admit(&name).await.ok_or_else(|| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "overloaded",
            format!("guest {name} is at capacity; retry later"),
        )
    })?;

    // Audit the invocation itself (every guest query is additionally audited by
    // the GuardedStore the mesh routes through).
    AuditLog::new(&state.store).record(&ctx, &format!("guest:{name}"))?;

    let mesh = state.mesh.clone();
    let request = GuestRequest::new(payload);
    let guest = name.clone();
    let response = tokio::task::spawn_blocking(move || mesh.invoke(&guest, &ctx, &request))
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                format!("guest task failed: {e}"),
            )
        })?
        .map_err(mesh_error)?;

    Ok(Json(response.payload))
}
