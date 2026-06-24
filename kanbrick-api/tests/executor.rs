//! #70 (Track F) — end-to-end control-plane / executor split.
//!
//! Wires a real control plane (internal RPC surface on loopback TCP) and a real
//! executor (also on loopback TCP, booted against the CP) in one process, then
//! drives the full path: public `POST /guests/{name}` → CP forwards to the
//! executor → executor runs the guest → the guest's `kbk_query_graph` callbacks
//! proxy back to the CP under a per-invocation capability → clearance-filtered
//! rows return. Covers the acceptance criteria:
//!
//! * same result as the in-process path, for a guest that calls `kbk_query_graph`
//!   (clearance filtering resolved via the cap — L5 sees detail, L1 does not);
//! * a forged capability on a callback is a CP `401` (⇒ guest trap, no data leak),
//!   and the executor cannot read above the clearance the CP bound to the cap;
//! * a CP activation propagates to the executor via the reconcile loop.
//!
//! Back-compat (no executor configured ⇒ byte-for-byte the prior single-pod
//! behaviour) is covered by the existing `e2e`/`http`/`registry` suites, which run
//! unchanged.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Duration as ChronoDuration;
use http_body_util::BodyExt;
use kanbrick_api::{
    build_executor, executor_router, internal_router, router, AdmissionConfig, ApiConfig, AppState,
    Executor, ExecutorConfig, RemoteHostServices,
};
use kanbrick_auth::{JwtAuthenticator, LoginService};
use kanbrick_core::abi::GraphQuery;
use kanbrick_core::{ClearanceLevel, FirmContext};
use kanbrick_mesh::HostServices;
use kanbrick_store::{seed, Migrator, Store};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const SECRET: &[u8] = b"executor-suite-secret";
const INTERNAL_TOKEN: &str = "executor-internal-token";

/// A real, compiled guest module — valid WASM for the activation/reconcile test.
const VALID_WASM: &[u8] = include_bytes!(env!("KANBRICK_VALUATION_GUEST_WASM"));

/// Everything a test needs: the CP state + public router, the live executor, and
/// the CP internal address. The `TempDir`s are held so the store/asset dirs
/// outlive the open handles.
struct Harness {
    _store_dir: tempfile::TempDir,
    _asset_dir: tempfile::TempDir,
    state: AppState,
    public: axum::Router,
    executor: Arc<Executor>,
    cp_internal_addr: SocketAddr,
}

/// Seed a firm + financials store, provision an L5 and an L1 login, then stand up
/// the CP internal surface and an executor against it — both on loopback TCP.
async fn setup() -> Harness {
    let store_dir = tempfile::tempdir().unwrap();
    let asset_dir = tempfile::tempdir().unwrap();
    let store = Store::open(store_dir.path()).unwrap();
    let firm = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../seed/kanbrick_seed_data.cypher"
    ))
    .unwrap();
    Migrator::firm(firm).run(&store).unwrap();
    let financials = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../seed/kanbrick_financials.cypher"
    ))
    .unwrap();
    seed::load_str(&store, &financials).unwrap();

    let jwt = JwtAuthenticator::new(SECRET, ChronoDuration::hours(1));
    {
        let svc = LoginService::new(&store, &jwt);
        svc.set_password("tracy.brittcool@kanbrick.com", "pw5")
            .unwrap();
        svc.set_password("dana.prescott@kanbrick.com", "pw1")
            .unwrap();
    }

    // Bind both listeners first so each side learns the other's address.
    let cp_internal_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cp_internal_addr = cp_internal_listener.local_addr().unwrap();
    let exec_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let exec_addr = exec_listener.local_addr().unwrap();

    let config = ApiConfig {
        admission: AdmissionConfig::default(),
        asset_dir: asset_dir.path().to_path_buf(),
        internal_token: Some(INTERNAL_TOKEN.to_string()),
        executor_url: Some(format!("http://{exec_addr}")),
    };
    let state = AppState::with_config(store, jwt, config).unwrap();

    // Serve the CP internal RPC surface (the executor's callback target).
    let internal_app = internal_router(state.clone());
    tokio::spawn(async move {
        axum::serve(cp_internal_listener, internal_app)
            .await
            .unwrap();
    });

    // Boot the executor against the CP (replays the registry), then serve it. The
    // listener is already bound, so the boot connection is accepted even before
    // the serve task above starts its accept loop.
    let exec_config = ExecutorConfig::new(format!("http://{cp_internal_addr}"), INTERNAL_TOKEN);
    let executor = tokio::task::spawn_blocking(move || build_executor(exec_config))
        .await
        .unwrap()
        .expect("executor boots against the control plane");
    let exec_app = executor_router(executor.clone());
    tokio::spawn(async move {
        axum::serve(exec_listener, exec_app).await.unwrap();
    });

    let public = router(state.clone());
    Harness {
        _store_dir: store_dir,
        _asset_dir: asset_dir,
        state,
        public,
        executor,
        cp_internal_addr,
    }
}

// ── Public-router helpers (oneshot; the forward path dials the executor over TCP) ──

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn login(app: &axum::Router, email: &str, password: &str) -> String {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "email": email, "password": password }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "login failed for {email}");
    body_json(resp).await["token"].as_str().unwrap().to_string()
}

async fn invoke(
    app: &axum::Router,
    token: &str,
    guest: &str,
    payload: Value,
) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/guests/{guest}"))
                .header("content-type", "application/json")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn upload(app: &axum::Router, token: &str, bytes: Vec<u8>) -> Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/assets/guests")
                .header("content-type", "application/wasm")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::from(bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    body_json(resp).await
}

async fn activate(app: &axum::Router, token: &str, name: &str, payload: Value) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/admin/guests/{name}/activate"))
                .header("content-type", "application/json")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

// ── Acceptance: same result as in-process, clearance resolved via the cap ────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reporting_through_executor_filters_by_clearance() {
    let h = setup().await;

    // L5 runs `reporting` on the executor; its `kbk_query_graph` callbacks resolve
    // to the L5 identity bound to the cap, so every company comes back in detail.
    let l5 = login(&h.public, "tracy.brittcool@kanbrick.com", "pw5").await;
    let (status, body) = invoke(&h.public, &l5, "reporting", json!({})).await;
    assert_eq!(status, StatusCode::OK, "L5 invoke via executor: {body}");
    let companies = body["companies"].as_array().unwrap();
    assert_eq!(companies.len(), 9, "the roster is public to all tiers");
    let l5_detail = companies
        .iter()
        .filter(|c| c.get("detail").is_some())
        .count();
    assert_eq!(
        l5_detail, 9,
        "L5 sees full detail through the executor callbacks"
    );

    // L1 runs the same guest; the executor cannot read above the clearance the CP
    // bound to the cap, so it sees the roster but no detail — identical to the
    // in-process path (cf. the e2e suite).
    let l1 = login(&h.public, "dana.prescott@kanbrick.com", "pw1").await;
    let (status, body) = invoke(&h.public, &l1, "reporting", json!({})).await;
    assert_eq!(status, StatusCode::OK, "L1 invoke via executor: {body}");
    let companies = body["companies"].as_array().unwrap();
    assert_eq!(companies.len(), 9, "roster still public");
    let l1_detail = companies
        .iter()
        .filter(|c| c.get("detail").is_some())
        .count();
    assert_eq!(
        l1_detail, 0,
        "L1 sees roster only — no detail leaks via the executor"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_guest_through_executor_is_404() {
    let h = setup().await;
    let l5 = login(&h.public, "tracy.brittcool@kanbrick.com", "pw5").await;
    // `valuation` is gated at L3 (the CP gate forwards), but the executor has no
    // guest named `nope` → the executor's 404 is relayed by the CP.
    let (status, _) = invoke(&h.public, &l5, "nope", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── Acceptance: a forged capability on a callback is rejected (⇒ trap) ───────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forged_capability_on_callback_is_rejected() {
    let h = setup().await;
    let cp_url = format!("http://{}", h.cp_internal_addr);

    // A remote backend (as the executor uses) presenting a bogus capability is
    // rejected by the CP (`401`). In a real run this Err becomes a guest trap, so
    // no rows ever cross the boundary.
    let remote = RemoteHostServices::new(cp_url.clone(), INTERNAL_TOKEN);
    let any_ctx = FirmContext::new(Uuid::new_v4(), "attacker@kanbrick.com", ClearanceLevel::L5);
    let query = GraphQuery::new("MATCH (c:Company) RETURN c.company_id");
    let forged = tokio::task::spawn_blocking(move || {
        remote.query_graph(&any_ctx, Some("forged-capability"), &query)
    })
    .await
    .unwrap();
    assert!(
        forged.is_err(),
        "a forged capability must not resolve to any rows"
    );

    // A real, minted capability bound to L5 resolves to all 9 companies — and the
    // `ctx` *argument* is ignored, proving identity is recovered server-side from
    // the cap, never trusted from the executor.
    let valid = h.state.caps.mint(
        FirmContext::new(
            Uuid::new_v4(),
            "tracy.brittcool@kanbrick.com",
            ClearanceLevel::L5,
        ),
        Duration::from_secs(60),
    );
    let remote = RemoteHostServices::new(cp_url, INTERNAL_TOKEN);
    let query = GraphQuery::new("MATCH (c:Company) RETURN c.company_id, c.name, c.description");
    let lying_ctx = FirmContext::new(Uuid::new_v4(), "ignored@kanbrick.com", ClearanceLevel::L1);
    let rows =
        tokio::task::spawn_blocking(move || remote.query_graph(&lying_ctx, Some(&valid), &query))
            .await
            .unwrap()
            .expect("a valid capability resolves");
    assert_eq!(
        rows.rows.len(),
        9,
        "rows reflect the L5 identity bound to the cap, not the L1 ctx argument"
    );
}

// ── Acceptance: a CP activation propagates to the executor via reconcile ─────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn activation_propagates_to_executor_via_reconcile() {
    let h = setup().await;

    // The executor boots with the three embedded guests; "shadow" is not present.
    assert!(h.executor.mesh().contains("reporting"));
    assert!(!h.executor.mesh().contains("shadow"));
    let generation_before = h.executor.generation();

    // Activate a new registry guest on the control plane.
    let l5 = login(&h.public, "tracy.brittcool@kanbrick.com", "pw5").await;
    let up = upload(&h.public, &l5, VALID_WASM.to_vec()).await;
    let uri = up["asset_uri"].as_str().unwrap().to_string();
    assert_eq!(
        activate(
            &h.public,
            &l5,
            "shadow",
            json!({ "asset_uri": uri, "version": "1.0.0", "min_clearance": "L3" }),
        )
        .await,
        StatusCode::OK
    );

    // Reconcile the executor: it observes the generation bump, pulls the asset,
    // and hot-loads the new guest.
    let exec = h.executor.clone();
    let changed = tokio::task::spawn_blocking(move || exec.reconcile_once())
        .await
        .unwrap()
        .expect("reconcile succeeds");
    assert!(changed, "a generation bump drives a reconcile");
    assert!(
        h.executor.mesh().contains("shadow"),
        "executor hot-loaded the newly activated guest"
    );
    assert!(h.executor.generation() > generation_before);

    // With no further activation, a second reconcile is a no-op.
    let exec = h.executor.clone();
    let changed = tokio::task::spawn_blocking(move || exec.reconcile_once())
        .await
        .unwrap()
        .unwrap();
    assert!(!changed, "an unchanged generation reconciles to nothing");
}
