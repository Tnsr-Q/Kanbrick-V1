//! P10.4 (#116) — `GET /me/components`. Asserts the visualizer's read surface
//! end-to-end: unauthenticated → 401, sub-clearance → 403, the registered
//! components are listed (with version + clearance floor), and live `GuestMetric`
//! counters are reflected after a real invocation.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Duration;
use http_body_util::BodyExt;
use kanbrick_api::{router, ApiConfig, AppState};
use kanbrick_auth::{JwtAuthenticator, LoginService};
use kanbrick_store::{seed, Migrator, Store};
use serde_json::{json, Value};
use tower::ServiceExt;

const SECRET: &[u8] = b"components-suite-secret";

const ELENA: &str = "elena.ruiz@kanbrick.com"; // L2 — below the L4 gate
const TRACY: &str = "tracy.brittcool@kanbrick.com"; // L5 — above the gate

/// Seed firm + financials (the valuation guest reads financials) with an L2
/// (`elena`) and L5 (`tracy`) login, matching the other API suites.
fn app() -> (tempfile::TempDir, axum::Router) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
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
    let jwt = JwtAuthenticator::new(SECRET, Duration::hours(1));
    {
        let svc = LoginService::new(&store, &jwt);
        svc.set_password(TRACY, "pw5").unwrap();
        svc.set_password(ELENA, "pw2").unwrap();
    }
    (dir, router(AppState::new(store, jwt).unwrap()))
}

async fn body(resp: axum::response::Response) -> Value {
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
    body(resp).await["token"].as_str().unwrap().to_string()
}

async fn get(app: &axum::Router, uri: &str, token: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(t) = token {
        builder = builder.header("Authorization", format!("Bearer {t}"));
    }
    let resp = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    (status, body(resp).await)
}

async fn invoke_guest(app: &axum::Router, name: &str, token: &str, payload: Value) -> StatusCode {
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

#[tokio::test]
async fn unauthenticated_is_rejected() {
    let (_d, app) = app();
    assert_eq!(
        get(&app, "/me/components", None).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn below_clearance_is_forbidden() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await; // L2 < L4
    assert_eq!(
        get(&app, "/me/components", Some(&elena)).await.0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn lists_registered_components_with_version_and_clearance() {
    let (_d, app) = app();
    let tracy = login(&app, TRACY, "pw5").await;

    let (status, list) = get(&app, "/me/components", Some(&tracy)).await;
    assert_eq!(status, StatusCode::OK);
    let arr = list.as_array().unwrap();

    // The three embedded guests, tagged kind=guest, sorted by name.
    let guest_names: Vec<&str> = arr
        .iter()
        .filter(|c| c["kind"] == "guest")
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(guest_names, ["compliance", "reporting", "valuation"]);

    // Clearance floors mirror the embedded guests' seeded policies.
    let by_name = |n: &str| arr.iter().find(|c| c["name"] == n).unwrap();
    assert_eq!(by_name("valuation")["clearance"], "L3");
    assert_eq!(by_name("compliance")["clearance"], "L4");
    assert_eq!(by_name("reporting")["clearance"], "L1");

    // Each guest carries a version and zeroed counters before any invocation.
    for c in arr.iter().filter(|c| c["kind"] == "guest") {
        assert!(c["version"].as_str().is_some(), "component missing version");
        assert_eq!(c["active"], 0);
        assert_eq!(c["completed"], 0);
        assert_eq!(c["failed"], 0);
        assert_eq!(c["timed_out"], 0);
    }
}

#[tokio::test]
async fn counters_reflect_guest_metric_state() {
    let (_d, app) = app();
    let tracy = login(&app, TRACY, "pw5").await;

    // Drive a real, successful valuation invocation (L3; tracy is L5).
    assert_eq!(
        invoke_guest(&app, "valuation", &tracy, json!({ "company_id": "JMTS" })).await,
        StatusCode::OK
    );

    let (status, list) = get(&app, "/me/components", Some(&tracy)).await;
    assert_eq!(status, StatusCode::OK);
    let valuation = list
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "valuation")
        .unwrap();
    assert_eq!(
        valuation["completed"], 1,
        "completed reflects the invocation"
    );
    assert_eq!(
        valuation["active"], 0,
        "the gauge returns to zero after the call"
    );
}

// ── In-process services (P10.7, #119) ───────────────────────────────────────

/// Like [`app`], but with the control-plane/executor split wired (an internal token
/// and an executor URL) so the conditional services light up. The executor URL is
/// never dialed — `/me/components` only reads `AppState`, it does not forward.
fn app_split() -> (tempfile::TempDir, axum::Router) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let firm = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../seed/kanbrick_seed_data.cypher"
    ))
    .unwrap();
    Migrator::firm(firm).run(&store).unwrap();
    let jwt = JwtAuthenticator::new(SECRET, Duration::hours(1));
    {
        let svc = LoginService::new(&store, &jwt);
        svc.set_password(TRACY, "pw5").unwrap();
    }
    let config = ApiConfig {
        internal_token: Some("split-suite-token".to_string()),
        executor_url: Some("http://executor.invalid:8090".to_string()),
        ..Default::default()
    };
    let state = AppState::with_config(store, jwt, config).unwrap();
    (dir, router(state))
}

#[tokio::test]
async fn in_process_services_appear_in_the_catalogue() {
    let (_d, app) = app();
    let tracy = login(&app, TRACY, "pw5").await;

    let (status, list) = get(&app, "/me/components", Some(&tracy)).await;
    assert_eq!(status, StatusCode::OK);
    let arr = list.as_array().unwrap();
    let by_name = |n: &str| arr.iter().find(|c| c["name"] == n);

    // Every core service is present, tagged kind=service, with a version + clearance.
    for svc in [
        "graph-store",
        "identity",
        "event-bus",
        "asset-store",
        "capability-registry",
        "provider-keys",
    ] {
        let row = by_name(svc).unwrap_or_else(|| panic!("missing service {svc}"));
        assert_eq!(row["kind"], "service", "{svc} is tagged as a service");
        assert!(row["version"].as_str().is_some(), "{svc} carries a version");
        assert!(
            row["clearance"].as_str().is_some(),
            "{svc} carries a clearance"
        );
        assert_eq!(row["active"], 0, "{svc} reports no invocation counters");
    }

    // Sensitive data planes sit at the L5 floor.
    assert_eq!(by_name("graph-store").unwrap()["clearance"], "L5");
    assert_eq!(by_name("capability-registry").unwrap()["clearance"], "L5");
}

#[tokio::test]
async fn service_set_reflects_live_configuration() {
    // Default config: no executor + no internal token ⇒ neither conditional service.
    let (_d, app) = app();
    let tracy = login(&app, TRACY, "pw5").await;
    let (_s, list) = get(&app, "/me/components", Some(&tracy)).await;
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"executor-forwarder"),
        "no forwarder service without an executor configured"
    );
    assert!(
        !names.contains(&"internal-rpc"),
        "no internal-rpc service without a transport token configured"
    );

    // Split config: an internal token + executor URL light up both services, so the
    // service set reflects the live AppState wiring.
    let (_d2, app2) = app_split();
    let tracy2 = login(&app2, TRACY, "pw5").await;
    let (status, list2) = get(&app2, "/me/components", Some(&tracy2)).await;
    assert_eq!(status, StatusCode::OK);
    let arr2 = list2.as_array().unwrap();
    let by_name = |n: &str| arr2.iter().find(|c| c["name"] == n);

    let forwarder =
        by_name("executor-forwarder").expect("forwarder appears when an executor is configured");
    assert_eq!(forwarder["kind"], "service");
    let rpc = by_name("internal-rpc").expect("internal-rpc appears when a token is configured");
    assert_eq!(rpc["kind"], "service");
    assert_eq!(rpc["clearance"], "L5");
}
