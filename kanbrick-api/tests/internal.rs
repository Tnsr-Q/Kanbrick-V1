//! #69 (Track E) — control-plane internal RPC surface + capability registry.
//!
//! Covers: the transport-secret gate (401 on missing/wrong token), capability
//! resolution + clearance-filtered graph reads, capability rejection (401 on
//! bogus/absent cap) for both graph and event callbacks, event publication onto
//! the shared bus, content-addressed asset fetch (200/404), and the registry
//! listing + generation increment on activation. All in-process; no executor.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chrono::Duration;
use http_body_util::BodyExt;
use kanbrick_api::{internal_router, router, AdmissionConfig, ApiConfig, AppState};
use kanbrick_auth::{JwtAuthenticator, LoginService};
use kanbrick_core::abi::{Event, GraphQuery, GraphRows};
use kanbrick_core::{ClearanceLevel, FirmContext};
use kanbrick_store::{Migrator, Store};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const SECRET: &[u8] = b"internal-suite-secret";
const INTERNAL_TOKEN: &str = "test-internal-token";

/// A real, compiled guest module — valid WASM for the activation test.
const VALID_WASM: &[u8] = include_bytes!(env!("KANBRICK_VALUATION_GUEST_WASM"));

/// Seed a fresh firm store with an L5 login, then build the shared state plus the
/// internal and public routers over it. The two `TempDir`s are returned so the
/// caller keeps them alive for the whole test — dropping them would delete the
/// store/asset directories out from under the open handles.
fn fresh() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    AppState,
    axum::Router,
    axum::Router,
) {
    let store_dir = tempfile::tempdir().unwrap();
    let asset_dir = tempfile::tempdir().unwrap();
    let store = Store::open(store_dir.path()).unwrap();
    let firm = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../seed/kanbrick_seed_data.cypher"
    ))
    .unwrap();
    Migrator::firm(firm).run(&store).unwrap();
    let jwt = JwtAuthenticator::new(SECRET, Duration::hours(1));
    {
        let svc = LoginService::new(&store, &jwt);
        svc.set_password("tracy.brittcool@kanbrick.com", "pw5")
            .unwrap();
    }
    let config = ApiConfig {
        admission: AdmissionConfig::default(),
        asset_dir: asset_dir.path().to_path_buf(),
        internal_token: Some(INTERNAL_TOKEN.to_string()),
        executor_url: None,
    };
    let state = AppState::with_config(store, jwt, config).unwrap();
    let internal = internal_router(state.clone());
    let public = router(state.clone());
    (store_dir, asset_dir, state, internal, public)
}

fn ctx(email: &str, clearance: ClearanceLevel) -> FirmContext {
    FirmContext::new(Uuid::new_v4(), email, clearance)
}

fn ttl() -> std::time::Duration {
    std::time::Duration::from_secs(60)
}

async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
    resp.into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

async fn body_json(resp: axum::response::Response) -> Value {
    serde_json::from_slice(&body_bytes(resp).await).unwrap_or(Value::Null)
}

async fn post_internal(
    app: &axum::Router,
    path: &str,
    token: Option<&str>,
    body: Value,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(t) = token {
        builder = builder.header("x-kanbrick-internal-token", t);
    }
    app.clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

async fn get_internal(
    app: &axum::Router,
    path: &str,
    token: Option<&str>,
) -> axum::response::Response {
    let mut builder = Request::builder().method("GET").uri(path);
    if let Some(t) = token {
        builder = builder.header("x-kanbrick-internal-token", t);
    }
    app.clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// A graph-read request body for the gated company-detail projection (the same
/// query the `guest_query` integration test uses): L3 sees 5 companies, L5 all 9.
fn detail_query_body(cap: &str) -> Value {
    json!({
        "cap": cap,
        "query": serde_json::to_value(GraphQuery::new(
            "MATCH (c:Company) RETURN c.company_id, c.name, c.description"
        )).unwrap(),
    })
}

// ── Public-router helpers (for the activation test) ─────────────────────────

async fn login(app: &axum::Router, email: &str, pw: &str) -> String {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "email": email, "password": pw }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    body_json(resp).await["token"].as_str().unwrap().to_string()
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

// ── Transport secret ────────────────────────────────────────────────────────

#[tokio::test]
async fn internal_surface_requires_the_transport_secret() {
    let (_sd, _ad, state, internal, _public) = fresh();
    let cap = state.caps.mint(
        ctx("tracy.brittcool@kanbrick.com", ClearanceLevel::L5),
        ttl(),
    );

    // A valid capability is still rejected without (or with the wrong) token.
    assert_eq!(
        post_internal(
            &internal,
            "/internal/graph/query",
            None,
            detail_query_body(&cap)
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post_internal(
            &internal,
            "/internal/graph/query",
            Some("wrong-token"),
            detail_query_body(&cap)
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    // GET routes are gated too.
    assert_eq!(
        get_internal(&internal, "/internal/registry", None)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

// ── Capability-gated graph reads ────────────────────────────────────────────

#[tokio::test]
async fn graph_query_resolves_capability_and_filters_by_clearance() {
    let (_sd, _ad, state, internal, _public) = fresh();

    // The same query, under two different capabilities, yields clearance-filtered
    // rows for the identity the control plane bound — not anything the caller says.
    let l3 = state.caps.mint(
        ctx("tyler.begemann@kanbrick.com", ClearanceLevel::L3),
        ttl(),
    );
    let resp = post_internal(
        &internal,
        "/internal/graph/query",
        Some(INTERNAL_TOKEN),
        detail_query_body(&l3),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let rows: GraphRows = serde_json::from_slice(&body_bytes(resp).await).unwrap();
    assert_eq!(
        rows.rows.len(),
        5,
        "L3 lead sees only their segment companies"
    );

    let l5 = state.caps.mint(
        ctx("tracy.brittcool@kanbrick.com", ClearanceLevel::L5),
        ttl(),
    );
    let resp = post_internal(
        &internal,
        "/internal/graph/query",
        Some(INTERNAL_TOKEN),
        detail_query_body(&l5),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let rows: GraphRows = serde_json::from_slice(&body_bytes(resp).await).unwrap();
    assert_eq!(rows.rows.len(), 9, "L5 CEO sees all companies");
}

#[tokio::test]
async fn callbacks_reject_an_invalid_capability() {
    let (_sd, _ad, _state, internal, _public) = fresh();

    // Graph read with a bogus capability → 401 (token is valid, cap is not).
    assert_eq!(
        post_internal(
            &internal,
            "/internal/graph/query",
            Some(INTERNAL_TOKEN),
            detail_query_body("not-a-real-capability")
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );

    // Event emit with a bogus capability → 401.
    let event = serde_json::to_value(Event::with_payload("x.kind", json!({}))).unwrap();
    assert_eq!(
        post_internal(
            &internal,
            "/internal/events",
            Some(INTERNAL_TOKEN),
            json!({ "cap": "nope", "event": event })
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}

// ── Event publication ───────────────────────────────────────────────────────

#[tokio::test]
async fn events_endpoint_publishes_to_the_shared_bus() {
    let (_sd, _ad, state, internal, _public) = fresh();
    let cap = state.caps.mint(
        ctx("tracy.brittcool@kanbrick.com", ClearanceLevel::L5),
        ttl(),
    );

    let event = serde_json::to_value(Event::with_payload(
        "valuation.completed",
        json!({ "company_id": "ACME" }),
    ))
    .unwrap();
    let resp = post_internal(
        &internal,
        "/internal/events",
        Some(INTERNAL_TOKEN),
        json!({ "cap": cap, "event": event }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // The event landed on the same bus the mesh is bound to (shared in AppState).
    let history = state.bus.history();
    assert!(
        history
            .iter()
            .any(|e| e.kind == "valuation.completed" && e.payload["company_id"] == json!("ACME")),
        "event should be published on the shared control-plane bus"
    );
}

// ── Content-addressed asset fetch ───────────────────────────────────────────

#[tokio::test]
async fn fetch_asset_round_trips_and_misses_404() {
    let (_sd, _ad, state, internal, _public) = fresh();
    let bytes = b"\0asm fake guest artifact".to_vec();
    let asset = state.assets.put(&bytes, None).unwrap();

    let resp = get_internal(
        &internal,
        &format!("/internal/assets/{}", asset.sha256),
        Some(INTERNAL_TOKEN),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/wasm"
    );
    assert_eq!(body_bytes(resp).await, bytes);

    // A well-formed but absent digest is a 404.
    let missing = "0".repeat(64);
    assert_eq!(
        get_internal(
            &internal,
            &format!("/internal/assets/{missing}"),
            Some(INTERNAL_TOKEN)
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}

// ── Registry listing + generation ───────────────────────────────────────────

#[tokio::test]
async fn registry_lists_embedded_guests_at_generation_zero() {
    let (_sd, _ad, _state, internal, _public) = fresh();
    let body =
        body_json(get_internal(&internal, "/internal/registry", Some(INTERNAL_TOKEN)).await).await;

    assert_eq!(body["generation"], 0, "no activation yet");
    let guests = body["guests"].as_array().unwrap();
    let names: Vec<&str> = guests.iter().map(|g| g["name"].as_str().unwrap()).collect();
    for guest in ["valuation", "reporting", "compliance"] {
        assert!(names.contains(&guest), "missing embedded guest {guest}");
    }
    let valuation = guests.iter().find(|g| g["name"] == "valuation").unwrap();
    assert_eq!(valuation["source"], "embedded");
    assert_eq!(valuation["min_clearance"], "L3");
}

#[tokio::test]
async fn registry_generation_increments_on_activation() {
    let (_sd, _ad, _state, internal, public) = fresh();

    let before =
        body_json(get_internal(&internal, "/internal/registry", Some(INTERNAL_TOKEN)).await).await;
    assert_eq!(before["generation"], 0);

    // Upload + activate a registry guest through the public admin surface.
    let l5 = login(&public, "tracy.brittcool@kanbrick.com", "pw5").await;
    let up = upload(&public, &l5, VALID_WASM.to_vec()).await;
    let uri = up["asset_uri"].as_str().unwrap().to_string();
    assert_eq!(
        activate(
            &public,
            &l5,
            "shadow",
            json!({ "asset_uri": uri, "version": "1.0.0", "min_clearance": "L1" }),
        )
        .await,
        StatusCode::OK
    );

    let after =
        body_json(get_internal(&internal, "/internal/registry", Some(INTERNAL_TOKEN)).await).await;
    assert_eq!(after["generation"], 1, "activation bumps the generation");
    let guests = after["guests"].as_array().unwrap();
    let shadow = guests
        .iter()
        .find(|g| g["name"] == "shadow")
        .expect("activated guest is listed");
    assert_eq!(shadow["source"], "registry");
    assert_eq!(shadow["version"], "1.0.0");
}

// ── Component self-registration (P10.6, #118) ───────────────────────────────

/// A `POST /internal/components/register` body for a sidecar/plugin descriptor.
fn registration_body(name: &str, version: &str, clearance: &str) -> Value {
    json!({ "name": name, "version": version, "clearance": clearance })
}

/// `GET /me/components` through the public router as an L4+ caller (the visualizer
/// read surface a self-registered component must surface in).
async fn me_components(public: &axum::Router, token: &str) -> Value {
    let resp = public
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/me/components")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    body_json(resp).await
}

#[tokio::test]
async fn component_registration_requires_the_transport_secret() {
    let (_sd, _ad, _state, internal, public) = fresh();

    // Registration is gated by the same shared secret: no token and a wrong token
    // both fail closed, with no JWT path to fall back on.
    assert_eq!(
        post_internal(
            &internal,
            "/internal/components/register",
            None,
            registration_body("ledger-sync", "2.1.0", "L3"),
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post_internal(
            &internal,
            "/internal/components/register",
            Some("wrong-token"),
            registration_body("ledger-sync", "2.1.0", "L3"),
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );

    // Nothing was registered: the visualizer shows only the embedded guests.
    let l5 = login(&public, "tracy.brittcool@kanbrick.com", "pw5").await;
    let list = me_components(&public, &l5).await;
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"ledger-sync"),
        "a tokenless registration must not appear in the catalogue"
    );
}

#[tokio::test]
async fn registered_component_appears_in_me_components() {
    let (_sd, _ad, _state, internal, public) = fresh();

    assert_eq!(
        post_internal(
            &internal,
            "/internal/components/register",
            Some(INTERNAL_TOKEN),
            registration_body("ledger-sync", "2.1.0", "L3"),
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );

    let l5 = login(&public, "tracy.brittcool@kanbrick.com", "pw5").await;
    let list = me_components(&public, &l5).await;
    let arr = list.as_array().unwrap();
    let by_name = |n: &str| arr.iter().find(|c| c["name"] == n);

    // No regression: the three embedded guests are still listed.
    for guest in ["valuation", "reporting", "compliance"] {
        assert!(by_name(guest).is_some(), "missing embedded guest {guest}");
    }

    // The self-registered sidecar appears with its descriptor and zeroed counters
    // (an externally-reported component carries no invocation metrics).
    let sidecar = by_name("ledger-sync").expect("registered component should be listed");
    assert_eq!(sidecar["version"], "2.1.0");
    assert_eq!(sidecar["clearance"], "L3");
    assert_eq!(sidecar["active"], 0);
    assert_eq!(sidecar["completed"], 0);
    assert_eq!(sidecar["failed"], 0);
    assert_eq!(sidecar["timed_out"], 0);
}

#[tokio::test]
async fn re_registration_replaces_the_descriptor() {
    let (_sd, _ad, _state, internal, public) = fresh();

    // Register the same name twice; the second registration refreshes the version.
    for version in ["1.0.0", "1.4.0"] {
        assert_eq!(
            post_internal(
                &internal,
                "/internal/components/register",
                Some(INTERNAL_TOKEN),
                registration_body("ledger-sync", version, "L3"),
            )
            .await
            .status(),
            StatusCode::NO_CONTENT
        );
    }

    let l5 = login(&public, "tracy.brittcool@kanbrick.com", "pw5").await;
    let list = me_components(&public, &l5).await;
    let matches: Vec<&Value> = list
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["name"] == "ledger-sync")
        .collect();
    assert_eq!(matches.len(), 1, "re-registration replaces, not duplicates");
    assert_eq!(matches[0]["version"], "1.4.0", "last registration wins");
}

#[tokio::test]
async fn a_live_guest_wins_a_name_collision_with_a_sidecar() {
    let (_sd, _ad, _state, internal, public) = fresh();

    // Register a sidecar that claims a real WASM guest's name with bogus metadata.
    assert_eq!(
        post_internal(
            &internal,
            "/internal/components/register",
            Some(INTERNAL_TOKEN),
            registration_body("valuation", "9.9.9", "L1"),
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );

    let l5 = login(&public, "tracy.brittcool@kanbrick.com", "pw5").await;
    let list = me_components(&public, &l5).await;
    let valuation: Vec<&Value> = list
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["name"] == "valuation")
        .collect();

    // The authoritative guest wins: one row, the guest's real clearance floor (L3),
    // and not the sidecar's spoofed version — a sidecar cannot shadow a real guest.
    assert_eq!(valuation.len(), 1, "a sidecar cannot duplicate a guest row");
    assert_eq!(
        valuation[0]["clearance"], "L3",
        "the guest's policy floor wins"
    );
    assert_ne!(
        valuation[0]["version"], "9.9.9",
        "the guest's version wins, not the sidecar's claim"
    );
}
