//! #63 (Track A) — `/metrics` endpoint: unauthenticated Prometheus exposition,
//! correct invocation accounting, and no sensitive labels.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chrono::Duration;
use http_body_util::BodyExt;
use kanbrick_api::{router, AppState};
use kanbrick_auth::{JwtAuthenticator, LoginService};
use kanbrick_store::{seed, Migrator, Store};
use serde_json::{json, Value};
use tower::ServiceExt;

/// Seed firm + financials with an L5 login, matching the other API suites.
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
    let jwt = JwtAuthenticator::new(b"metrics-suite-secret", Duration::hours(1));
    {
        let svc = LoginService::new(&store, &jwt);
        svc.set_password("tracy.brittcool@kanbrick.com", "pw5")
            .unwrap();
    }
    (dir, router(AppState::new(store, jwt).unwrap()))
}

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
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
    let json: Value = serde_json::from_str(&body_string(resp).await).unwrap();
    json["token"].as_str().unwrap().to_string()
}

async fn get_metrics(app: &axum::Router) -> (StatusCode, String, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    (status, content_type, body_string(resp).await)
}

#[tokio::test]
async fn metrics_is_unauthenticated_and_prometheus() {
    let (_d, app) = app();
    let (status, content_type, body) = get_metrics(&app).await;

    // No Authorization header required.
    assert_eq!(status, StatusCode::OK);
    assert!(
        content_type.contains("text/plain"),
        "content-type was {content_type:?}"
    );

    // The three embedded guests are listed from boot, before any invocation.
    for guest in ["valuation", "reporting", "compliance"] {
        assert!(
            body.contains(&format!(
                "kanbrick_guest_invocations_active{{guest=\"{guest}\"}}"
            )),
            "missing active series for {guest}"
        );
    }
    // The KEDA signal and TYPE headers are present.
    assert!(body.contains("# TYPE kanbrick_mesh_pressure_ratio gauge"));
    assert!(body.contains("kanbrick_mesh_pressure_ratio "));
    assert!(body.contains("# TYPE kanbrick_guest_invocations_total counter"));
}

#[tokio::test]
async fn successful_invocation_increments_completed() {
    let (_d, app) = app();
    let l5 = login(&app, "tracy.brittcool@kanbrick.com", "pw5").await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/guests/valuation")
                .header("content-type", "application/json")
                .header("Authorization", format!("Bearer {l5}"))
                .body(Body::from(json!({ "company_id": "JMTS" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let (_s, _ct, body) = get_metrics(&app).await;
    assert!(
        body.contains(
            "kanbrick_guest_invocations_total{guest=\"valuation\",result=\"completed\"} 1"
        ),
        "valuation completed counter should be 1; body:\n{body}"
    );
    // The gauge returns to zero once the call has finished.
    assert!(body.contains("kanbrick_guest_invocations_active{guest=\"valuation\"} 0"));
}

#[tokio::test]
async fn metrics_leak_no_identities() {
    let (_d, app) = app();
    // Drive an authenticated invocation so a session exists, then scrape.
    let l5 = login(&app, "tracy.brittcool@kanbrick.com", "pw5").await;
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/guests/valuation")
                .header("content-type", "application/json")
                .header("Authorization", format!("Bearer {l5}"))
                .body(Body::from(json!({ "company_id": "JMTS" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    let (_s, _ct, body) = get_metrics(&app).await;
    // Non-sensitive: guest names and result labels only — no emails, no tokens.
    assert!(
        !body.contains('@'),
        "metrics must not expose email identities"
    );
    assert!(
        !body.to_lowercase().contains("bearer"),
        "metrics must not expose tokens"
    );
}
