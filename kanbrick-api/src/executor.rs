//! Executor run-mode + remote host services + control-plane forwarding (#70,
//! Track F).
//!
//! This is where the control-plane / executor split actually happens. WASM
//! execution is the CPU-heavy, untrusted, horizontally-scalable part of an
//! invocation; the graph and asset registry stay behind the single control-plane
//! (CP) writer. So:
//!
//! * **Executor mode** runs guest WASM on a stateless, disposable pool. It has no
//!   store, no JWT, and no public surface. A guest's `kbk_query_graph` /
//!   `kbk_emit_event` host calls are serviced by [`RemoteHostServices`], which
//!   proxies them back to the CP's internal RPC surface (#69), authorized by a
//!   per-invocation **capability**. Identity is never sent or trusted over the
//!   wire — the CP recovers it server-side from the capability.
//! * **CP forwarding** ([`ExecutorClient`]) hands an authenticated, cleared,
//!   admitted, audited invocation to the executor pool and relays the result.
//!
//! The split is **fully backwards-compatible**: with no executor configured the
//! CP runs guests in-process exactly as before (see `invoke_guest`).
//!
//! ## Security
//!
//! The executor relays only the opaque capability on a callback. A compromised
//! executor (or a WASM escape) therefore cannot act above the clearance the CP
//! bound to the invocation: a forged or expired capability is a CP `401`, which
//! surfaces to the guest as a trap (no data leak). This extends ADR-0002's
//! host-authoritative identity across the network hop (#69/#70).
//!
//! Both the executor's `/internal/invoke` and the CP's internal RPC surface are
//! gated by the same shared transport secret and are **ClusterIP-only** — never
//! exposed through the public ingress (#71 enforces this with NetworkPolicy).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{from_fn_with_state, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use kanbrick_core::abi::{Event, GraphQuery, GraphRows, GuestRequest, GuestResponse};
use kanbrick_core::FirmContext;
use kanbrick_mesh::{HostServices, HostServicesError, MeshRuntime};
use kanbrick_store::SOURCE_REGISTRY;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

use crate::admission::{AdmissionConfig, GuestAdmission};
use crate::http_client::{self, HttpResponse};
use crate::internal::{ct_eq, RegistryResponse, HEADER_INTERNAL_TOKEN};
use crate::{mesh_error, ApiError};

/// The content-address URI scheme the asset registry uses (#64). Mirrors
/// `kanbrick_mesh::assets`'s scheme; restated here as the stable wire prefix.
const ASSET_URI_PREFIX: &str = "tachyon://sha256:";

/// Per-request timeout for control-plane calls made by an executor (boot replay,
/// reconcile, and the graph/event callbacks).
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the executor polls the CP registry for a generation bump.
pub const DEFAULT_RECONCILE_INTERVAL: Duration = Duration::from_secs(15);

// ── Errors ──────────────────────────────────────────────────────────────────

/// A failure talking to the control plane from the executor side.
#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    /// The HTTP request to the control plane could not be completed.
    #[error("control-plane request failed: {0}")]
    Transport(String),
    /// The control plane returned a non-success status.
    #[error("control-plane returned status {status}: {body}")]
    Status {
        /// The HTTP status code returned.
        status: u16,
        /// The (possibly truncated) response body, for diagnosis.
        body: String,
    },
    /// A response body could not be decoded, or an asset URI was malformed.
    #[error("control-plane response could not be decoded: {0}")]
    Decode(String),
    /// Building the mesh or compiling an embedded guest failed.
    #[error("mesh error: {0}")]
    Mesh(String),
}

// ── Wire types ──────────────────────────────────────────────────────────────

/// `POST /internal/invoke` body — the CP-to-executor invocation envelope.
///
/// `ctx` travels here **only** to feed the guest's read-only `kbk_ctx_*` imports;
/// it is never trusted on a callback. Callbacks are authorized by `cap`, which the
/// CP minted and bound to the authoritative identity.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct InvokeRequest {
    /// The guest to run.
    pub(crate) guest: String,
    /// The guest-specific request payload.
    pub(crate) request: GuestRequest,
    /// The host-authoritative caller context (read-only, for `kbk_ctx_*`).
    pub(crate) ctx: FirmContext,
    /// The per-invocation capability authorizing this run's callbacks.
    pub(crate) cap: String,
}

// ── Control-plane client (executor → CP internal RPC) ───────────────────────

/// The `content-type` sent on internal-RPC request bodies (all JSON).
const JSON_CONTENT_TYPE: &str = "application/json";

/// A blocking client for the CP's internal RPC surface (#69), over the
/// dependency-free `http_client`. Cheap to clone. In-cluster, plain HTTP. An
/// internal detail of executor wiring — not part of the crate's public API.
#[derive(Clone)]
pub(crate) struct CpClient {
    base_url: String,
    token: String,
    timeout: Duration,
}

impl CpClient {
    /// Build a client for the control plane at `base_url`, presenting `token` as
    /// the shared transport secret on every request.
    pub(crate) fn new(
        base_url: impl Into<String>,
        token: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        CpClient {
            base_url: trim_base(base_url.into()),
            token: token.into(),
            timeout,
        }
    }

    /// Issue a request to the CP internal surface, returning the framed response.
    fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, ExecutorError> {
        let url = format!("{}{}", self.base_url, path);
        let headers: &[(&str, &str)] = &[
            (HEADER_INTERNAL_TOKEN, self.token.as_str()),
            ("content-type", JSON_CONTENT_TYPE),
        ];
        http_client::request(method, &url, headers, body, self.timeout)
            .map_err(|e| ExecutorError::Transport(e.to_string()))
    }

    /// Fetch the activated-guest listing and current registry generation.
    pub(crate) fn get_registry(&self) -> Result<RegistryResponse, ExecutorError> {
        let resp = self.send("GET", "/internal/registry", None)?;
        let body = ok_body(resp)?;
        serde_json::from_slice::<RegistryResponse>(&body)
            .map_err(|e| ExecutorError::Decode(e.to_string()))
    }

    /// Fetch a content-addressed guest artifact by its `tachyon://sha256:<hex>`
    /// URI, returning the raw bytes (re-verified by the CP on read).
    pub(crate) fn get_asset(&self, asset_uri: &str) -> Result<Vec<u8>, ExecutorError> {
        let sha = asset_uri.strip_prefix(ASSET_URI_PREFIX).ok_or_else(|| {
            ExecutorError::Decode(format!("asset uri is not content-addressed: {asset_uri}"))
        })?;
        let resp = self.send("GET", &format!("/internal/assets/{sha}"), None)?;
        ok_body(resp)
    }

    /// Run a guest's graph read on the CP under the capability's bound clearance.
    fn query_graph(&self, cap: &str, query: &GraphQuery) -> Result<GraphRows, ExecutorError> {
        let body = serde_json::to_vec(&json!({ "cap": cap, "query": query }))
            .map_err(|e| ExecutorError::Decode(e.to_string()))?;
        let resp = self.send("POST", "/internal/graph/query", Some(&body))?;
        let body = ok_body(resp)?;
        serde_json::from_slice::<GraphRows>(&body).map_err(|e| ExecutorError::Decode(e.to_string()))
    }

    /// Publish a guest-emitted event onto the CP bus, authorized by `cap`.
    fn emit_event(&self, cap: &str, event: &Event) -> Result<(), ExecutorError> {
        let body = serde_json::to_vec(&json!({ "cap": cap, "event": event }))
            .map_err(|e| ExecutorError::Decode(e.to_string()))?;
        let resp = self.send("POST", "/internal/events", Some(&body))?;
        ok_body(resp).map(|_| ())
    }
}

/// Return the body of a successful response, or map a non-success status to
/// [`ExecutorError::Status`] (preserving the code — e.g. `401` for a bad cap).
fn ok_body(resp: HttpResponse) -> Result<Vec<u8>, ExecutorError> {
    if resp.is_success() {
        Ok(resp.body)
    } else {
        Err(ExecutorError::Status {
            status: resp.status,
            body: String::from_utf8_lossy(&resp.body).into_owned(),
        })
    }
}

/// Strip a trailing `/` from a base URL so path joins don't double up.
fn trim_base(mut url: String) -> String {
    while url.ends_with('/') {
        url.pop();
    }
    url
}

// ── Remote host services (the #68 HostServices seam, executor side) ─────────

/// A [`HostServices`] backend that proxies a guest's graph reads and event emits
/// back to the control plane over the internal RPC surface, authorized by the
/// per-invocation capability. This is what the executor binds via
/// [`MeshRuntime::with_services`](kanbrick_mesh::MeshRuntime::with_services).
///
/// The caller `ctx` is **ignored** on callbacks: the CP recovers identity from
/// the capability, so a compromised executor cannot name a different identity. A
/// missing capability (the in-process `None`) is a programming error in executor
/// mode and is surfaced as a query/emit failure (⇒ guest trap).
#[derive(Clone)]
pub struct RemoteHostServices {
    cp: CpClient,
}

impl RemoteHostServices {
    /// Build a remote backend targeting the control plane at `cp_url`, using
    /// `token` as the shared transport secret.
    pub fn new(cp_url: impl Into<String>, token: impl Into<String>) -> Self {
        RemoteHostServices {
            cp: CpClient::new(cp_url, token, DEFAULT_REQUEST_TIMEOUT),
        }
    }
}

impl HostServices for RemoteHostServices {
    fn query_graph(
        &self,
        _ctx: &FirmContext,
        cap: Option<&str>,
        query: &GraphQuery,
    ) -> Result<GraphRows, HostServicesError> {
        let cap = cap.ok_or_else(|| {
            HostServicesError::Query("no capability bound to this invocation".to_string())
        })?;
        self.cp
            .query_graph(cap, query)
            .map_err(|e| HostServicesError::Query(e.to_string()))
    }

    fn emit_event(
        &self,
        _ctx: &FirmContext,
        cap: Option<&str>,
        event: &Event,
    ) -> Result<(), HostServicesError> {
        let cap = cap.ok_or_else(|| {
            HostServicesError::Emit("no capability bound to this invocation".to_string())
        })?;
        self.cp
            .emit_event(cap, event)
            .map_err(|e| HostServicesError::Emit(e.to_string()))
    }
}

// ── Executor ────────────────────────────────────────────────────────────────

/// Configuration for an executor pod.
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Base URL of the control plane's internal RPC surface (e.g.
    /// `http://kanbrick-cp:8090`).
    pub cp_url: String,
    /// Shared transport secret presented to the CP and required on the
    /// executor's own `/internal/invoke`.
    pub internal_token: String,
    /// Per-guest admission limits for this pod (the pressure KEDA reads, #63/#71).
    pub admission: AdmissionConfig,
    /// Per-request timeout for control-plane calls.
    pub request_timeout: Duration,
}

impl ExecutorConfig {
    /// Build a config with default admission limits and request timeout.
    pub fn new(cp_url: impl Into<String>, internal_token: impl Into<String>) -> Self {
        ExecutorConfig {
            cp_url: cp_url.into(),
            internal_token: internal_token.into(),
            admission: AdmissionConfig::default(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

/// A stateless guest executor: a mesh backed by [`RemoteHostServices`], the
/// embedded guests, and any registry-activated guests replayed from the CP.
pub struct Executor {
    mesh: Arc<MeshRuntime>,
    admission: Arc<GuestAdmission>,
    /// The shared transport secret this executor requires on `/internal/invoke`.
    token: Arc<str>,
    /// Client for reconciling registry deltas from the control plane.
    cp: CpClient,
    /// The last registry generation this executor has reconciled to.
    generation: AtomicU64,
}

impl Executor {
    /// The executor's mesh (for diagnostics/tests).
    pub fn mesh(&self) -> &MeshRuntime {
        &self.mesh
    }

    /// The registry generation this executor has reconciled to.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Poll the CP registry once; on a generation bump, re-pull every registry
    /// guest and hot-reload it, then advance to the new generation. Returns
    /// whether anything was reconciled. Idempotent: an unchanged generation is a
    /// no-op, and a reload replaces in place (in-flight calls are unaffected).
    pub fn reconcile_once(&self) -> Result<bool, ExecutorError> {
        let registry = self.cp.get_registry()?;
        if registry.generation <= self.generation.load(Ordering::Relaxed) {
            return Ok(false);
        }
        apply_registry(&self.mesh, &self.cp, &registry, "reconcile");
        self.generation
            .store(registry.generation, Ordering::Relaxed);
        Ok(true)
    }
}

/// Load every registry-source guest in `registry` into `mesh` by fetching its
/// asset from the CP and (hot-)reloading it. Failures are logged and skipped,
/// leaving any prior module in place — one bad guest never wedges the pool.
fn apply_registry(mesh: &MeshRuntime, cp: &CpClient, registry: &RegistryResponse, phase: &str) {
    for guest in &registry.guests {
        if guest.source != SOURCE_REGISTRY {
            continue;
        }
        match cp.get_asset(&guest.asset_uri) {
            Ok(bytes) => match mesh.reload_module(&guest.name, &guest.version, &bytes) {
                Ok(()) => tracing::info!(
                    target: "kanbrick_api::executor",
                    guest = %guest.name,
                    version = %guest.version,
                    phase,
                    "loaded registry guest"
                ),
                Err(e) => tracing::error!(
                    target: "kanbrick_api::executor",
                    guest = %guest.name,
                    error = %e,
                    phase,
                    "registry guest failed to compile; leaving prior module in place"
                ),
            },
            Err(e) => tracing::error!(
                target: "kanbrick_api::executor",
                guest = %guest.name,
                uri = %guest.asset_uri,
                error = %e,
                phase,
                "registry asset unavailable; skipping"
            ),
        }
    }
}

/// Build an executor: bind [`RemoteHostServices`] to a fresh mesh, register the
/// embedded business guests from the binary, then replay any registry-activated
/// guests from the control plane and record its generation.
///
/// Performs blocking HTTP to the control plane (registry + asset fetches); call
/// it off the async runtime (e.g. via `spawn_blocking`).
pub fn build_executor(config: ExecutorConfig) -> Result<Arc<Executor>, ExecutorError> {
    let cp = CpClient::new(
        &config.cp_url,
        &config.internal_token,
        config.request_timeout,
    );
    let remote = RemoteHostServices { cp: cp.clone() };
    let mut mesh = MeshRuntime::new()
        .map_err(|e| ExecutorError::Mesh(e.to_string()))?
        .with_services(Arc::new(remote));

    // Embedded guests ship in the binary — same artifacts the control plane has.
    for guest in crate::EMBEDDED_GUESTS {
        mesh.register_module(guest.name, crate::GUEST_VERSION, guest.wasm)
            .map_err(|e| ExecutorError::Mesh(e.to_string()))?;
    }

    // Replay the registry-activated guests from the control plane.
    let registry = cp.get_registry()?;
    apply_registry(&mesh, &cp, &registry, "boot");
    let generation = registry.generation;

    let admission = Arc::new(GuestAdmission::new(
        mesh.guests().into_iter().map(|g| g.name),
        config.admission,
    ));

    tracing::info!(
        target: "kanbrick_api::executor",
        cp_url = %cp.base_url,
        guests = mesh.guests().len(),
        generation,
        "executor booted"
    );

    Ok(Arc::new(Executor {
        mesh: Arc::new(mesh),
        admission,
        token: Arc::from(config.internal_token.as_str()),
        cp,
        generation: AtomicU64::new(generation),
    }))
}

/// Spawn the background reconcile loop on a dedicated thread: every `interval`
/// it polls the CP registry and reloads on a generation bump. Returns the join
/// handle (the loop runs until the process exits).
pub fn spawn_reconcile_loop(
    executor: Arc<Executor>,
    interval: Duration,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("executor-reconcile".to_string())
        .spawn(move || loop {
            std::thread::sleep(interval);
            match executor.reconcile_once() {
                Ok(true) => tracing::info!(
                    target: "kanbrick_api::executor",
                    generation = executor.generation(),
                    "reconciled to a new registry generation"
                ),
                Ok(false) => {}
                Err(e) => tracing::warn!(
                    target: "kanbrick_api::executor",
                    error = %e,
                    "registry reconcile poll failed; will retry"
                ),
            }
        })
        .expect("spawn executor reconcile thread")
}

// ── Executor HTTP surface ────────────────────────────────────────────────────

/// Build the executor's HTTP router: a transport-secret-gated `/internal/invoke`,
/// plus ungated `/metrics` (#63) and `/health`. There is no store, no JWT, and no
/// public surface here.
pub fn executor_router(executor: Arc<Executor>) -> Router {
    Router::new()
        .route(
            "/internal/invoke",
            post(invoke).route_layer(from_fn_with_state(executor.clone(), require_executor_token)),
        )
        .route("/metrics", get(metrics))
        .route("/health", get(health))
        .with_state(executor)
}

/// Reject any `/internal/invoke` request without the shared transport secret.
/// Fails closed and compares in constant time, exactly like the CP surface (#69).
async fn require_executor_token(
    State(executor): State<Arc<Executor>>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let provided = req
        .headers()
        .get(HEADER_INTERNAL_TOKEN)
        .and_then(|v| v.to_str().ok());
    let ok = match provided {
        Some(got) => ct_eq(executor.token.as_bytes(), got.as_bytes()),
        None => false,
    };
    if ok {
        Ok(next.run(req).await)
    } else {
        Err(ApiError::unauthorized("missing or invalid internal token"))
    }
}

/// `POST /internal/invoke` — run a guest under the supplied host-authoritative
/// `ctx` (read-only) with the per-invocation `cap` threaded to the remote host
/// services. Admission-bounded so a single pod sheds load past its queue (`429`).
async fn invoke(
    State(executor): State<Arc<Executor>>,
    Json(req): Json<InvokeRequest>,
) -> Result<Json<GuestResponse>, ApiError> {
    let _permit = executor.admission.admit(&req.guest).await.ok_or_else(|| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "overloaded",
            format!("guest {} is at capacity; retry later", req.guest),
        )
    })?;

    let mesh = executor.mesh.clone();
    let InvokeRequest {
        guest,
        request,
        ctx,
        cap,
    } = req;
    let response = tokio::task::spawn_blocking(move || {
        mesh.invoke_with_cap(&guest, &ctx, Some(&cap), &request)
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            format!("guest task failed: {e}"),
        )
    })?
    .map_err(mesh_error)?;

    Ok(Json(response))
}

/// `GET /metrics` — Prometheus exposition of this pod's guest pressure (#63).
async fn metrics(State(executor): State<Arc<Executor>>) -> Response {
    let body = crate::metrics::render_prometheus(
        &executor.mesh.metrics_snapshot(),
        &executor.admission.snapshot(),
    );
    ([(header::CONTENT_TYPE, crate::metrics::CONTENT_TYPE)], body).into_response()
}

/// `GET /health` — liveness with the loaded-guest count and reconciled generation.
async fn health(State(executor): State<Arc<Executor>>) -> Json<JsonValue> {
    Json(json!({
        "status": "healthy",
        "mode": "executor",
        "guests_loaded": executor.mesh.guests().len(),
        "generation": executor.generation(),
        "version": crate::GUEST_VERSION,
    }))
}

// ── Control-plane forwarder (CP → executor) ─────────────────────────────────

/// The CP-side client that forwards an invocation to the executor pool, over the
/// dependency-free `http_client`. Driven from `invoke_guest` via
/// `spawn_blocking`. In-cluster, plain HTTP.
pub struct ExecutorClient {
    base_url: String,
    token: Arc<str>,
}

impl ExecutorClient {
    /// Build a forwarder targeting the executor Service at `base_url`, presenting
    /// `token` as the shared transport secret.
    pub fn new(base_url: impl Into<String>, token: Arc<str>) -> Self {
        ExecutorClient {
            base_url: trim_base(base_url.into()),
            token,
        }
    }

    /// Forward one invocation to the executor pool and return its response.
    /// Transport failures map to `502`; an executor error response is relayed with
    /// its original status preserved (e.g. `404` unknown guest, `500` guest trap).
    pub fn invoke(
        &self,
        guest: &str,
        ctx: &FirmContext,
        cap: &str,
        request: &GuestRequest,
    ) -> Result<GuestResponse, ApiError> {
        let envelope = InvokeRequest {
            guest: guest.to_string(),
            request: request.clone(),
            ctx: ctx.clone(),
            cap: cap.to_string(),
        };
        let body = serde_json::to_vec(&envelope).map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                format!("encoding invoke envelope: {e}"),
            )
        })?;
        let url = format!("{}/internal/invoke", self.base_url);
        let headers: &[(&str, &str)] = &[
            (HEADER_INTERNAL_TOKEN, self.token.as_ref()),
            ("content-type", JSON_CONTENT_TYPE),
        ];
        let resp =
            http_client::request("POST", &url, headers, Some(&body), DEFAULT_REQUEST_TIMEOUT)
                .map_err(|e| {
                    ApiError::new(
                        StatusCode::BAD_GATEWAY,
                        "bad_gateway",
                        format!("executor unreachable: {e}"),
                    )
                })?;
        if resp.is_success() {
            serde_json::from_slice::<GuestResponse>(&resp.body).map_err(|e| {
                ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "bad_gateway",
                    format!("invalid executor response: {e}"),
                )
            })
        } else {
            Err(relay_error(
                resp.status,
                String::from_utf8_lossy(&resp.body).into_owned(),
            ))
        }
    }
}

/// Translate an executor error response into an [`ApiError`] that preserves the
/// upstream status and message (parsed from the `{ "error": { .. } }` envelope
/// when present), so the client sees the same failure it would in-process.
fn relay_error(status: u16, body: String) -> ApiError {
    let message = serde_json::from_str::<JsonValue>(&body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| {
            if body.is_empty() {
                format!("executor returned status {status}")
            } else {
                body
            }
        });
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let kind = match code {
        StatusCode::NOT_FOUND => "not_found",
        StatusCode::FORBIDDEN => "forbidden",
        StatusCode::UNAUTHORIZED => "unauthorized",
        StatusCode::TOO_MANY_REQUESTS => "overloaded",
        StatusCode::BAD_REQUEST => "invalid_request",
        _ => "internal",
    };
    ApiError::new(code, kind, message)
}
