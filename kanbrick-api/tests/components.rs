//! P10.4 (#116) — `GET /me/components`. Asserts the visualizer's read surface
//! end-to-end: unauthenticated → 401, sub-clearance → 403, the registered
//! components are listed (with version + clearance floor), and live `GuestMetric`
//! counters are reflected after a real invocation.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Duration;
use http_body_util::BodyExt;
use kanbrick_api::{router, AppState};
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

    // The three embedded guests, sorted by name.
    let names: Vec<&str> = arr.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["compliance", "reporting", "valuation"]);

    // Clearance floors mirror the embedded guests' seeded policies.
    let by_name = |n: &str| arr.iter().find(|c| c["name"] == n).unwrap();
    assert_eq!(by_name("valuation")["clearance"], "L3");
    assert_eq!(by_name("compliance")["clearance"], "L4");
    assert_eq!(by_name("reporting")["clearance"], "L1");

    // Each carries a version and zeroed counters before any invocation.
    for c in arr {
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
