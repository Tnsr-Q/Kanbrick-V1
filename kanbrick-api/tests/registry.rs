//! #64 (Track C) — air-gapped guest registry + SparrowDB-backed policy.
//!
//! Covers: upload auth (401/403), empty body, hash mismatch, valid upload,
//! the clearance floor, invalid-WASM activation (preserves the old guest, writes
//! no policy), valid hot-reload activation, boot replay across a restart, and
//! audit entries for upload + activation.

use std::path::Path;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Duration;
use http_body_util::BodyExt;
use kanbrick_api::{router, AdmissionConfig, ApiConfig, AppState};
use kanbrick_auth::{JwtAuthenticator, LoginService};
use kanbrick_store::{read_guest_policy, seed, Migrator, Params, Store};
use serde_json::{json, Value};
use tower::ServiceExt;

const SECRET: &[u8] = b"registry-suite-secret";

/// A real, compiled guest module — valid WASM for activation tests. (The api
/// build script exports this path so test code can embed it too.)
const VALID_WASM: &[u8] = include_bytes!(env!("KANBRICK_VALUATION_GUEST_WASM"));

/// Build a router + state over an already-open store, with a temp asset volume.
fn build(store: Store, asset_dir: &Path) -> (AppState, axum::Router) {
    let jwt = JwtAuthenticator::new(SECRET, Duration::hours(1));
    let config = ApiConfig {
        admission: AdmissionConfig::default(),
        asset_dir: asset_dir.to_path_buf(),
    };
    let state = AppState::with_config(store, jwt, config).unwrap();
    let app = router(state.clone());
    (state, app)
}

/// Seed a fresh store (firm + financials + L5/L2 logins) and build the app.
fn fresh(store_dir: &Path, asset_dir: &Path) -> (AppState, axum::Router) {
    let store = Store::open(store_dir).unwrap();
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
    {
        let jwt = JwtAuthenticator::new(SECRET, Duration::hours(1));
        let svc = LoginService::new(&store, &jwt);
        svc.set_password("tracy.brittcool@kanbrick.com", "pw5")
            .unwrap();
        svc.set_password("elena.ruiz@kanbrick.com", "pw2").unwrap();
    }
    build(store, asset_dir)
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn login(app: &axum::Router, email: &str, pw: &str) -> String {
    let b = json!({ "email": email, "password": pw }).to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/login")
                .header("content-type", "application/json")
                .body(Body::from(b))
                .unwrap(),
        )
        .await
        .unwrap();
    body_json(resp).await["token"].as_str().unwrap().to_string()
}

async fn upload(
    app: &axum::Router,
    token: Option<&str>,
    bytes: Vec<u8>,
    expected_sha: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/admin/assets/guests")
        .header("content-type", "application/wasm");
    if let Some(t) = token {
        builder = builder.header("Authorization", format!("Bearer {t}"));
    }
    if let Some(sha) = expected_sha {
        builder = builder.header("x-kanbrick-expected-sha256", sha);
    }
    let resp = app
        .clone()
        .oneshot(builder.body(Body::from(bytes)).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn activate(
    app: &axum::Router,
    token: Option<&str>,
    name: &str,
    payload: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/admin/guests/{name}/activate"))
        .header("content-type", "application/json");
    if let Some(t) = token {
        builder = builder.header("Authorization", format!("Bearer {t}"));
    }
    let resp = app
        .clone()
        .oneshot(builder.body(Body::from(payload.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn invoke(app: &axum::Router, name: &str, token: &str, payload: Value) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/guests/{name}"))
                .header("content-type", "application/json")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn guests_loaded(app: &axum::Router) -> u64 {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    body_json(resp).await["guests_loaded"].as_u64().unwrap()
}

fn audit_count(state: &AppState) -> i64 {
    state
        .store
        .scalar_i64("MATCH (a:AuditEntry) RETURN count(a)", Params::new())
        .unwrap()
        .unwrap_or(0)
}

// ── Upload auth + validation ────────────────────────────────────────────────

#[tokio::test]
async fn upload_requires_l5() {
    let (sd, ad) = (tmp(), tmp());
    let (_state, app) = fresh(sd.path(), ad.path());

    // No token → 401.
    let (status, _) = upload(&app, None, VALID_WASM.to_vec(), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Genuine L2 token → 403.
    let l2 = login(&app, "elena.ruiz@kanbrick.com", "pw2").await;
    let (status, body) = upload(&app, Some(&l2), VALID_WASM.to_vec(), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["kind"], "forbidden");
}

#[tokio::test]
async fn upload_rejects_empty_body_and_hash_mismatch() {
    let (sd, ad) = (tmp(), tmp());
    let (_state, app) = fresh(sd.path(), ad.path());
    let l5 = login(&app, "tracy.brittcool@kanbrick.com", "pw5").await;

    let (status, body) = upload(&app, Some(&l5), Vec::new(), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["kind"], "invalid_request");

    let (status, body) = upload(
        &app,
        Some(&l5),
        VALID_WASM.to_vec(),
        Some("0000000000000000000000000000000000000000000000000000000000000000"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["kind"], "hash_mismatch");
}

#[tokio::test]
async fn valid_l5_upload_stores_and_returns_uri() {
    let (sd, ad) = (tmp(), tmp());
    let (state, app) = fresh(sd.path(), ad.path());
    let l5 = login(&app, "tracy.brittcool@kanbrick.com", "pw5").await;

    let before = audit_count(&state);
    let (status, body) = upload(&app, Some(&l5), VALID_WASM.to_vec(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["stored"], true);
    let uri = body["asset_uri"].as_str().unwrap();
    assert!(uri.starts_with("tachyon://sha256:"));
    assert_eq!(body["sha256"].as_str().unwrap().len(), 64);
    // Idempotent: re-upload yields the same content address.
    let (_s, body2) = upload(&app, Some(&l5), VALID_WASM.to_vec(), None).await;
    assert_eq!(body2["asset_uri"], body["asset_uri"]);
    // Upload is audited.
    assert!(audit_count(&state) > before, "upload writes an audit entry");
}

// ── Activation auth, floor, failure, success ────────────────────────────────

#[tokio::test]
async fn activate_requires_l5() {
    let (sd, ad) = (tmp(), tmp());
    let (_state, app) = fresh(sd.path(), ad.path());
    let l2 = login(&app, "elena.ruiz@kanbrick.com", "pw2").await;

    let payload =
        json!({ "asset_uri": "tachyon://sha256:abc", "version": "1", "min_clearance": "L1" });
    assert_eq!(
        activate(&app, None, "shadow", payload.clone()).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        activate(&app, Some(&l2), "shadow", payload).await.0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn activation_cannot_lower_embedded_floor() {
    let (sd, ad) = (tmp(), tmp());
    let (state, app) = fresh(sd.path(), ad.path());
    let l5 = login(&app, "tracy.brittcool@kanbrick.com", "pw5").await;

    // valuation's embedded floor is L3; L2 must be rejected before any work.
    let (status, body) = activate(
        &app,
        Some(&l5),
        "valuation",
        json!({ "asset_uri": "tachyon://sha256:abc", "version": "9", "min_clearance": "L2" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["kind"], "invalid_request");

    // Policy is untouched: still the embedded L3.
    let policy = read_guest_policy(&state.store, "valuation")
        .unwrap()
        .unwrap();
    assert_eq!(policy.min_clearance, kanbrick_core::ClearanceLevel::L3);
    assert_eq!(policy.source, kanbrick_store::SOURCE_EMBEDDED);
}

#[tokio::test]
async fn invalid_wasm_activation_preserves_guest_and_writes_no_policy() {
    let (sd, ad) = (tmp(), tmp());
    let (state, app) = fresh(sd.path(), ad.path());
    let l5 = login(&app, "tracy.brittcool@kanbrick.com", "pw5").await;

    // Upload garbage (stored fine; only activation compiles it).
    let (_s, up) = upload(&app, Some(&l5), b"\0 definitely not wasm".to_vec(), None).await;
    let bad_uri = up["asset_uri"].as_str().unwrap().to_string();

    // Activate valuation (floor L3, so L3 passes the floor) with the bad artifact.
    let (status, _) = activate(
        &app,
        Some(&l5),
        "valuation",
        json!({ "asset_uri": bad_uri, "version": "9.9.9", "min_clearance": "L3" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "bad wasm fails to compile"
    );

    // The embedded guest is untouched: policy still embedded, version unchanged.
    let policy = read_guest_policy(&state.store, "valuation")
        .unwrap()
        .unwrap();
    assert_eq!(policy.source, kanbrick_store::SOURCE_EMBEDDED);
    assert_ne!(
        policy.version, "9.9.9",
        "no policy was written for the failed activation"
    );

    // And it still serves.
    assert_eq!(
        invoke(&app, "valuation", &l5, json!({ "company_id": "JMTS" })).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn valid_activation_registers_and_persists_policy() {
    let (sd, ad) = (tmp(), tmp());
    let (state, app) = fresh(sd.path(), ad.path());
    let l5 = login(&app, "tracy.brittcool@kanbrick.com", "pw5").await;

    assert_eq!(
        guests_loaded(&app).await,
        3,
        "three embedded guests at boot"
    );

    let (_s, up) = upload(&app, Some(&l5), VALID_WASM.to_vec(), None).await;
    let uri = up["asset_uri"].as_str().unwrap().to_string();

    let before = audit_count(&state);
    let (status, body) = activate(
        &app,
        Some(&l5),
        "shadow",
        json!({ "asset_uri": uri, "version": "1.0.0", "min_clearance": "L1" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["activated"], true);

    // The new guest is registered and policy persisted as a registry override.
    assert_eq!(
        guests_loaded(&app).await,
        4,
        "activated guest is now loaded"
    );
    let policy = read_guest_policy(&state.store, "shadow").unwrap().unwrap();
    assert!(policy.is_registry());
    assert_eq!(policy.version, "1.0.0");
    assert!(
        audit_count(&state) > before,
        "activation writes an audit entry"
    );

    // It is invokable (L1 floor; the valuation logic errors on a bad payload but
    // the guest runs — proving registration, i.e. not a 404).
    assert_ne!(
        invoke(&app, "shadow", &l5, json!({ "company_id": "JMTS" })).await,
        StatusCode::NOT_FOUND
    );
}

// ── Boot replay across a restart ────────────────────────────────────────────

#[tokio::test]
async fn registry_guests_replay_after_restart() {
    let sd = tmp();
    let ad = tmp();

    // First boot: upload + activate a registry guest, then checkpoint the store.
    {
        let (state, app) = fresh(sd.path(), ad.path());
        let l5 = login(&app, "tracy.brittcool@kanbrick.com", "pw5").await;
        let (_s, up) = upload(&app, Some(&l5), VALID_WASM.to_vec(), None).await;
        let uri = up["asset_uri"].as_str().unwrap().to_string();
        let (status, _) = activate(
            &app,
            Some(&l5),
            "shadow",
            json!({ "asset_uri": uri, "version": "1.0.0", "min_clearance": "L1" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        state.store.checkpoint().unwrap();
        // Drop both handles so the GraphDb file lock is released before reopen.
        drop(app);
        drop(state);
    }

    // Restart: reopen the same store + asset volume and boot a new app. The
    // registry guest must be replayed from SparrowDB policy + asset bytes.
    let store = Store::open(sd.path()).unwrap();
    let (state, app) = build(store, ad.path());
    assert_eq!(
        guests_loaded(&app).await,
        4,
        "registry guest replayed at boot"
    );
    let policy = read_guest_policy(&state.store, "shadow").unwrap().unwrap();
    assert!(policy.is_registry());
    assert_eq!(policy.version, "1.0.0");
}
