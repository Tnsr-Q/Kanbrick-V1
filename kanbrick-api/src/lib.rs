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
//! * `POST /admin/assets/guests` — L5: store a guest WASM artifact in the
//!   content-addressed registry (#64).
//! * `POST /admin/guests/{name}/activate` — L5: bind a guest to a stored artifact
//!   and hot-reload it (#64).
//! * `POST/GET/DELETE /me/provider-keys` — per-employee BYO-AI provider-key
//!   custody, clearance-gated + audited, with metadata-only reads (#103).
//!
//! The three business guests are **embedded** in the binary at build time
//! (`include_bytes!`), so a single self-contained binary serves them (#53). The
//! caller's identity is supplied to a guest *only* by the host-authoritative mesh
//! (`kbk_ctx_*`); it is never taken from the request payload, so it cannot be
//! forged.
//!
//! A missing/invalid/expired JWT yields a structured `401`; insufficient
//! clearance yields a structured `403`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use kanbrick_auth::{require_clearance, AuditLog, JwtAuthenticator, LoginService};
use kanbrick_core::abi::GuestRequest;
use kanbrick_core::{ClearanceLevel, Error, ErrorKind, FirmContext};
use kanbrick_mesh::{AssetError, AssetStore, EventBus, MeshError, MeshRuntime, Scheduler};
use kanbrick_providers::{InMemoryKeyStore, ProviderKeyStore};
use kanbrick_store::{
    bump_registry_generation, list_guest_policies, read_guest_policy, write_guest_policy,
    GuestPolicy, Store, SOURCE_EMBEDDED, SOURCE_REGISTRY,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

mod admission;
mod caps;
mod components;
mod executor;
mod grants;
mod http_client;
mod internal;
mod loops;
mod messenger;
mod metrics;
mod provider_keys;
mod skills;

pub use admission::{AdmissionConfig, GuestAdmission};
pub use caps::InvocationCaps;
pub use components::{ComponentRegistry, RegisteredComponent};
pub use executor::{
    build_executor, executor_router, register_component, spawn_reconcile_loop, Executor,
    ExecutorClient, ExecutorConfig, ExecutorError, RemoteHostServices, DEFAULT_RECONCILE_INTERVAL,
};
pub use internal::internal_router;
pub use loops::LoopRunRegistry;

/// Default location of the content-addressed asset volume in containers (#64).
pub const DEFAULT_ASSET_DIR: &str = "/var/lib/kanbrick/assets";

/// Lifetime of a per-invocation capability the control plane mints when
/// forwarding to an executor (#70). It need only outlast a single invocation's
/// graph/event callbacks; the cap is revoked the moment the invocation returns.
const CAP_TTL: Duration = Duration::from_secs(60);

/// Capacity of the control-plane event bus's in-memory replay log (#114). The bus
/// keeps a bounded recent-replay window so its log cannot grow without limit;
/// durable history (e.g. the messenger's `(:MessengerMessage)` records) lives in
/// the store, not this window.
const EVENT_LOG_CAPACITY: usize = 1024;

/// The three business guests, embedded at build time (build.rs → `include_bytes!`).
const VALUATION_WASM: &[u8] = include_bytes!(env!("KANBRICK_VALUATION_GUEST_WASM"));
const REPORTING_WASM: &[u8] = include_bytes!(env!("KANBRICK_REPORTING_GUEST_WASM"));
const COMPLIANCE_WASM: &[u8] = include_bytes!(env!("KANBRICK_COMPLIANCE_GUEST_WASM"));

/// Guest version reported in the registry (the API crate version).
const GUEST_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A guest baked into the binary. Its `min_clearance` is the **floor** the
/// registry may never drop below for that name (#64).
struct EmbeddedGuest {
    name: &'static str,
    wasm: &'static [u8],
    min_clearance: ClearanceLevel,
}

/// The boot-embedded business guests and their clearance floors. Mirrors each
/// guest's own `REQUIRED_CLEARANCE`: valuation L3, compliance L4; reporting is
/// clearance-*tiered* so any authenticated caller (L1) may run it.
const EMBEDDED_GUESTS: &[EmbeddedGuest] = &[
    EmbeddedGuest {
        name: "valuation",
        wasm: VALUATION_WASM,
        min_clearance: ClearanceLevel::L3,
    },
    EmbeddedGuest {
        name: "compliance",
        wasm: COMPLIANCE_WASM,
        min_clearance: ClearanceLevel::L4,
    },
    EmbeddedGuest {
        name: "reporting",
        wasm: REPORTING_WASM,
        min_clearance: ClearanceLevel::L1,
    },
];

/// The embedded clearance floor for `name`, if it is an embedded guest. Registry
/// activations may raise a guest's clearance but never set it below this.
fn embedded_floor(name: &str) -> Option<ClearanceLevel> {
    EMBEDDED_GUESTS
        .iter()
        .find(|g| g.name == name)
        .map(|g| g.min_clearance)
}

/// API configuration (#63 admission limits + #64 asset volume + #69 internal RPC).
#[derive(Debug, Clone)]
pub struct ApiConfig {
    /// Per-guest admission limits.
    pub admission: AdmissionConfig,
    /// Root of the content-addressed guest asset volume.
    pub asset_dir: PathBuf,
    /// Shared transport secret guarding the internal RPC surface (#69). `None`
    /// disables it — every `/internal/*` request fails closed.
    pub internal_token: Option<String>,
    /// Base URL of the executor pool's `/internal/invoke` surface (#70). When set
    /// (with an `internal_token`), guest invocations are forwarded to the executor
    /// pool; when unset, the control plane runs guests in-process as before.
    pub executor_url: Option<String>,
}

impl Default for ApiConfig {
    fn default() -> Self {
        ApiConfig {
            admission: AdmissionConfig::default(),
            asset_dir: PathBuf::from(DEFAULT_ASSET_DIR),
            internal_token: None,
            executor_url: None,
        }
    }
}

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
    /// Content-addressed store for runtime-activated guest artifacts (#64).
    pub assets: Arc<AssetStore>,
    /// The control-plane event bus, shared with the mesh so in-process guest
    /// emits and executor `/internal/events` callbacks land on the same bus (#69).
    pub bus: EventBus,
    /// Per-invocation capability registry for the internal RPC surface (#69).
    pub caps: Arc<InvocationCaps>,
    /// Self-registered sidecar/plugin components surfaced in the visualizer
    /// (`/me/components`, P10.6, #118). Populated over the internal RPC surface.
    pub components: ComponentRegistry,
    /// Shared transport secret guarding the internal RPC surface (#69); `None`
    /// disables it.
    pub internal_token: Option<Arc<str>>,
    /// Forwarder to the executor pool (#70). `Some` means guest invocations are
    /// proxied to executors; `None` means they run in-process on this node.
    pub executor: Option<Arc<ExecutorClient>>,
    /// Per-employee provider-key custody (P9.3, #103), namespaced by `user_id`.
    /// Defaults to an in-memory store; the cockpit injects the Stronghold-backed
    /// backend (ADR-0009) via [`with_provider_keys`](AppState::with_provider_keys).
    pub provider_keys: Arc<dyn ProviderKeyStore>,
    /// The loop run engine's scheduler (P11.3), wrapping the same `mesh` so loop
    /// steps run as scheduled guest invocations with per-guest concurrency + timeouts.
    pub scheduler: Arc<Scheduler>,
    /// In-process loop-run history surfaced by `GET /me/loops/runs/{id}` (P11.3).
    /// Durable run persistence is P11.5.
    pub loop_runs: LoopRunRegistry,
}

impl AppState {
    /// Build state from a store and JWT authenticator with default config.
    pub fn new(store: Store, jwt: JwtAuthenticator) -> Result<Self, Error> {
        Self::with_config(store, jwt, ApiConfig::default())
    }

    /// Like [`new`](Self::new) but with explicit admission limits and asset volume.
    ///
    /// Boots the mesh: registers the embedded guests, seeds their policies, and
    /// replays any registry-activated guests from the asset volume (#64).
    pub fn with_config(
        store: Store,
        jwt: JwtAuthenticator,
        config: ApiConfig,
    ) -> Result<Self, Error> {
        let store = Arc::new(store);
        let assets = Arc::new(AssetStore::new(config.asset_dir));
        // Bounded recent-replay window so the bus log cannot grow without limit
        // (#114); durable history lives in the store, not this window.
        let bus = EventBus::with_capacity(EVENT_LOG_CAPACITY);
        let mesh = Arc::new(build_mesh(store.clone(), &assets, bus.clone())?);
        // The loop run engine (P11.3) schedules guest invocations on this same mesh.
        let scheduler = Arc::new(Scheduler::new(mesh.clone()));
        let admission = Arc::new(GuestAdmission::new(
            mesh.guests().into_iter().map(|g| g.name),
            config.admission,
        ));
        let internal_token: Option<Arc<str>> = config.internal_token.map(|t| Arc::from(t.as_str()));
        // Wire the executor forwarder when both an executor URL and the shared
        // transport secret are configured (#70). A URL without a token is a
        // misconfiguration (the executor would reject the CP's calls): warn and
        // fall back to in-process execution rather than failing every invoke.
        let executor = match (config.executor_url, internal_token.clone()) {
            (Some(url), Some(token)) => Some(Arc::new(ExecutorClient::new(url, token))),
            (Some(_), None) => {
                tracing::warn!(
                    "KANBRICK_EXECUTOR_URL is set but no internal token is configured; \
                     executor forwarding disabled (running guests in-process)"
                );
                None
            }
            (None, _) => None,
        };
        Ok(AppState {
            store,
            jwt: Arc::new(jwt),
            mesh,
            admission,
            assets,
            bus,
            caps: Arc::new(InvocationCaps::new()),
            components: ComponentRegistry::new(),
            internal_token,
            executor,
            provider_keys: Arc::new(InMemoryKeyStore::new()),
            scheduler,
            loop_runs: LoopRunRegistry::new(),
        })
    }

    /// Replace the provider-key custody backend. The cockpit injects the
    /// Stronghold-backed store here (ADR-0009); a builder, so existing call sites
    /// are unchanged and default to the in-memory store.
    pub fn with_provider_keys(mut self, provider_keys: Arc<dyn ProviderKeyStore>) -> Self {
        self.provider_keys = provider_keys;
        self
    }
}

/// Build the mesh runtime, bound to the firm graph (so `query_graph` works) and an
/// event bus (so guest `emit` works), then:
///
/// 1. register the embedded guests and seed a `GuestPolicy` for each that has none
///    (preserving any prior registry override across restarts);
/// 2. replay registry-activated guests by loading their bytes from the asset store
///    and hot-reloading them. A missing/corrupt/invalid artifact is logged and
///    skipped, leaving the embedded guest in place.
fn build_mesh(store: Arc<Store>, assets: &AssetStore, bus: EventBus) -> Result<MeshRuntime, Error> {
    let mut mesh = MeshRuntime::new()?.with_store(store.clone()).with_bus(bus);

    for guest in EMBEDDED_GUESTS {
        mesh.register_module(guest.name, GUEST_VERSION, guest.wasm)?;
        if read_guest_policy(&store, guest.name)?.is_none() {
            write_guest_policy(
                &store,
                &GuestPolicy::new(
                    guest.name,
                    GUEST_VERSION,
                    guest.min_clearance,
                    "",
                    SOURCE_EMBEDDED,
                ),
            )?;
        }
    }

    for policy in list_guest_policies(&store)? {
        if !policy.is_registry() {
            continue;
        }
        match assets.get(&policy.asset_uri) {
            Ok(bytes) => {
                if let Err(e) = mesh.reload_module(&policy.guest_name, &policy.version, &bytes) {
                    tracing::error!(
                        target: "kanbrick_api::registry",
                        guest = %policy.guest_name,
                        error = %e,
                        "registry guest failed to compile at boot; leaving prior module in place"
                    );
                } else {
                    tracing::info!(
                        target: "kanbrick_api::registry",
                        guest = %policy.guest_name,
                        version = %policy.version,
                        "replayed registry guest from asset store"
                    );
                }
            }
            Err(e) => tracing::error!(
                target: "kanbrick_api::registry",
                guest = %policy.guest_name,
                uri = %policy.asset_uri,
                error = %e,
                "registry asset unavailable at boot; skipping"
            ),
        }
    }

    Ok(mesh)
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
        .route("/admin/assets/guests", post(upload_asset))
        .route("/admin/guests/{name}/activate", post(activate_guest))
        .route(
            "/me/provider-keys",
            post(provider_keys::create_key).get(provider_keys::list_keys),
        )
        .route("/me/provider-keys/{id}", delete(provider_keys::delete_key))
        .route("/me/messenger/send", post(messenger::send_message))
        .route("/me/messenger/log", get(messenger::message_log))
        .route("/me/components", get(components::list_components))
        .route("/me/scope-requests", post(grants::create_scope_request))
        .route("/me/scope-requests/{id}", get(grants::read_scope_request))
        .route(
            "/me/scope-requests/{id}/approve",
            post(grants::approve_scope_request),
        )
        .route(
            "/me/scope-requests/{id}/deny",
            post(grants::deny_scope_request),
        )
        .route("/me/scopes", get(grants::list_scopes))
        .route("/me/scopes/{id}/revoke", post(grants::revoke_scope))
        .route(
            "/me/scopes/{id}/skills",
            post(skills::bind_skill).get(skills::list_scope_skills),
        )
        .route(
            "/me/skills",
            post(skills::publish_skill).get(skills::browse_skills),
        )
        .route("/me/skills/{name}", get(skills::skill_history))
        .route(
            "/me/loops",
            post(loops::create_loop_handler).get(loops::list_loops_handler),
        )
        .route("/me/loops/{id}", get(loops::get_loop_handler))
        .route("/me/loops/{id}/run", post(loops::run_loop_handler))
        .route("/me/loops/runs/{id}", get(loops::get_run_handler))
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

/// Map an asset-store failure onto an API error. Client mistakes (empty body,
/// bad hash, malformed/absent URI) are `4xx`; integrity and I/O faults are `500`.
fn asset_error(err: AssetError) -> ApiError {
    match err {
        AssetError::Empty => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "asset body is empty",
        ),
        AssetError::HashMismatch { expected, actual } => ApiError::new(
            StatusCode::BAD_REQUEST,
            "hash_mismatch",
            format!("expected sha256 {expected}, computed {actual}"),
        ),
        AssetError::InvalidUri(uri) => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            format!("invalid asset uri {uri}"),
        ),
        AssetError::NotFound(uri) => ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("asset {uri} not found"),
        ),
        AssetError::Corrupt { uri, .. } => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            format!("asset {uri} failed its integrity check"),
        ),
        AssetError::Io(detail) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", detail)
        }
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
    // The minimum clearance is the state-backed policy (#64), seeded from the
    // embedded guests and extended/raised by registry activations.
    let policy = read_guest_policy(&state.store, &name)?.ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("unknown guest {name}"),
        )
    })?;
    require_clearance(&ctx, policy.min_clearance)?;

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
    // the GuardedStore the mesh routes through). Auth, clearance, admission, and
    // audit all stay on the control plane regardless of where the guest runs.
    AuditLog::new(&state.store).record(&ctx, &format!("guest:{name}"))?;

    let request = GuestRequest::new(payload);

    // Executor split (#70): if an executor pool is configured, mint a single-
    // invocation capability bound to the host-authoritative `ctx`, forward the run
    // to the pool, and revoke the cap the moment it returns. The executor relays
    // only the opaque cap on callbacks — identity is never trusted over the wire.
    // With no executor configured, fall through to in-process execution
    // (byte-for-byte the prior single-pod behaviour).
    if let Some(executor) = state.executor.clone() {
        let cap = state.caps.mint(ctx.clone(), CAP_TTL);
        let guest = name.clone();
        let forward_ctx = ctx.clone();
        let forward_cap = cap.clone();
        let forwarded = tokio::task::spawn_blocking(move || {
            executor.invoke(&guest, &forward_ctx, &forward_cap, &request)
        })
        .await;
        // The cap's window is exactly this invocation; revoke it regardless of
        // outcome so a leaked token cannot be replayed afterward.
        state.caps.revoke(&cap);
        let response = forwarded.map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                format!("forward task failed: {e}"),
            )
        })??;
        return Ok(Json(response.payload));
    }

    let mesh = state.mesh.clone();
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

/// Header carrying the caller's expected SHA-256 of an uploaded asset.
const HEADER_EXPECTED_SHA: &str = "x-kanbrick-expected-sha256";

/// `POST /admin/assets/guests` — store a guest WASM artifact in the content-
/// addressed registry (#64). **Requires L5.**
///
/// Body is the raw `application/wasm` bytes. The optional `x-kanbrick-expected-sha256`
/// header is verified against the bytes (mismatch ⇒ `400`). The artifact is named
/// by its content address; the upload is idempotent.
async fn upload_asset(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<JsonValue>, ApiError> {
    require_clearance(&ctx, ClearanceLevel::L5)?;
    let expected = headers
        .get(HEADER_EXPECTED_SHA)
        .and_then(|v| v.to_str().ok());
    let asset = state.assets.put(&body, expected).map_err(asset_error)?;
    AuditLog::new(&state.store).record(&ctx, &format!("asset:upload:{}", asset.sha256))?;
    Ok(Json(serde_json::json!({
        "asset_uri": asset.uri,
        "sha256": asset.sha256,
        "stored": true,
    })))
}

/// `POST /admin/guests/{name}/activate` request body.
#[derive(Debug, Deserialize)]
pub struct ActivateRequest {
    /// Content-addressed URI of a previously-uploaded artifact.
    pub asset_uri: String,
    /// Version to record for the activated guest.
    pub version: String,
    /// Minimum clearance to invoke the guest (e.g. `"L3"`).
    pub min_clearance: ClearanceLevel,
}

/// `POST /admin/guests/{name}/activate` — bind a guest name to a stored artifact
/// and hot-reload it (#64). **Requires L5.**
///
/// The clearance floor is enforced: an embedded guest may be raised but never set
/// below its baseline. The swap is compile-first and atomic — if the artifact
/// fails to compile, the previously-active guest keeps serving and **no** policy
/// is written.
async fn activate_guest(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Path(name): Path<String>,
    Json(req): Json<ActivateRequest>,
) -> Result<Json<JsonValue>, ApiError> {
    require_clearance(&ctx, ClearanceLevel::L5)?;

    // Privilege floor: registry activation may raise but never lower an embedded
    // guest's clearance.
    if let Some(floor) = embedded_floor(&name) {
        if req.min_clearance < floor {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                format!(
                    "min_clearance {} is below the embedded floor {floor} for guest {name}",
                    req.min_clearance
                ),
            ));
        }
    }

    // Fetch + integrity-check the bytes, then compile-first swap. Order matters:
    // nothing is persisted unless the new module actually compiles.
    let bytes = state.assets.get(&req.asset_uri).map_err(asset_error)?;
    state
        .mesh
        .reload_module(&name, &req.version, &bytes)
        .map_err(mesh_error)?;

    let policy = GuestPolicy::new(
        &name,
        &req.version,
        req.min_clearance,
        &req.asset_uri,
        SOURCE_REGISTRY,
    );
    write_guest_policy(&state.store, &policy)?;
    // Bump the persisted registry generation so executors (#70) detect the change
    // and reconcile (re-pull the asset + hot-reload).
    bump_registry_generation(&state.store)?;
    AuditLog::new(&state.store).record(&ctx, &format!("guest:activate:{name}:{}", req.version))?;

    Ok(Json(serde_json::json!({
        "guest": name,
        "version": req.version,
        "min_clearance": req.min_clearance,
        "asset_uri": req.asset_uri,
        "activated": true,
    })))
}
