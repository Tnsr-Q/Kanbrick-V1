//! #47 — end-to-end suite: full lifecycle at all five clearance levels.
//!
//! Seeds a fresh system, provisions a representative user per tier (L1–L5),
//! logs each in over HTTP, and exercises every guest at its permitted tiers
//! through `POST /guests/{name}` — asserting known-correct seed outputs and the
//! clearance rejections. The whole path is real: HTTP → Auth → Mesh → Guest →
//! Graph (the embedded guests run as wasm under the host-authoritative context).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Duration;
use http_body_util::BodyExt;
use kanbrick_api::{router, AppState};
use kanbrick_auth::{JwtAuthenticator, LoginService};
use kanbrick_store::{seed, Migrator, Store};
use serde_json::{json, Value};
use tower::ServiceExt;

/// A representative login per clearance tier, with its password.
const USERS: &[(&str, &str, &str)] = &[
    ("L5", "tracy.brittcool@kanbrick.com", "pw-l5"),
    ("L4", "andrea.lewis@kanbrick.com", "pw-l4"),
    ("L3", "tyler.begemann@kanbrick.com", "pw-l3"),
    ("L2", "elena.ruiz@kanbrick.com", "pw-l2"),
    ("L1", "dana.prescott@kanbrick.com", "pw-l1"),
];

/// Seed the firm graph + synthetic financials and provision all five logins.
fn seeded_app() -> (tempfile::TempDir, axum::Router) {
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

    let jwt = JwtAuthenticator::new(b"e2e-secret", Duration::hours(1));
    {
        let svc = LoginService::new(&store, &jwt);
        for (_, email, pw) in USERS {
            svc.set_password(email, pw).unwrap();
        }
    }
    (dir, router(AppState::new(store, jwt).unwrap()))
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn login(app: &axum::Router, email: &str, password: &str) -> String {
    let body = json!({ "email": email, "password": password }).to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/login")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "login failed for {email}");
    body_json(resp).await["token"].as_str().unwrap().to_string()
}

/// Invoke a guest over HTTP, returning `(status, body)`.
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

#[tokio::test]
async fn sessions_for_every_clearance_level() {
    let (_d, app) = seeded_app();
    // A session (login + /me) works for one user at each of the five tiers.
    for (level, email, pw) in USERS {
        let token = login(&app, email, pw).await;
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/me")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["clearance"], json!(level));
    }
}

#[tokio::test]
async fn health_reports_three_embedded_guests() {
    let (_d, app) = seeded_app();
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["status"], json!("healthy"));
    assert_eq!(body["guests_loaded"], json!(3));
}

#[tokio::test]
async fn reporting_is_clearance_tiered_for_every_user() {
    let (_d, app) = seeded_app();
    // Reporting runs at every tier; the roster is public (9) but detail varies.
    let mut detail_counts = Vec::new();
    for (_, email, pw) in USERS {
        let token = login(&app, email, pw).await;
        let (status, body) = invoke(&app, token.as_str(), "reporting", json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["companies"].as_array().unwrap().len(),
            9,
            "roster public to all"
        );
        let with_detail = body["companies"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c.get("detail").is_some())
            .count();
        detail_counts.push(with_detail);
    }
    // L5/L4 see all 9 in detail; lower tiers see fewer — verifiably different.
    assert_eq!(detail_counts[0], 9, "L5 full detail");
    assert_eq!(detail_counts[1], 9, "L4 full detail");
    assert_eq!(detail_counts[4], 0, "L1 roster only");
    assert!(detail_counts[0] != detail_counts[4]);
}

#[tokio::test]
async fn valuation_permitted_for_l3_plus_forbidden_below() {
    let (_d, app) = seeded_app();

    // L3 lead can value a company in their segment → a real DCF report.
    let l3 = login(&app, "tyler.begemann@kanbrick.com", "pw-l3").await;
    let (status, body) = invoke(&app, &l3, "valuation", json!({"company_id": "JMTS"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["company_id"], json!("JMTS"));
    assert!(body["enterprise_value"].as_f64().unwrap() > 0.0);

    // L2 analyst is forbidden at the API gate (valuation requires L3+).
    let l2 = login(&app, "elena.ruiz@kanbrick.com", "pw-l2").await;
    let (status, body) = invoke(&app, &l2, "valuation", json!({"company_id": "JMTS"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["kind"], json!("forbidden"));
}

#[tokio::test]
async fn compliance_permitted_for_l4_plus_forbidden_below() {
    let (_d, app) = seeded_app();

    // L5 CEO runs the compliance check → the seed graph passes cleanly.
    let l5 = login(&app, "tracy.brittcool@kanbrick.com", "pw-l5").await;
    let (status, body) = invoke(&app, &l5, "compliance", json!({"check": "all"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["passed"], json!(true));
    assert!(body["violations"].as_array().unwrap().is_empty());

    // L3 lead is forbidden (compliance requires L4+).
    let l3 = login(&app, "tyler.begemann@kanbrick.com", "pw-l3").await;
    let (status, _) = invoke(&app, &l3, "compliance", json!({"check": "all"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn unknown_guest_is_404() {
    let (_d, app) = seeded_app();
    let l5 = login(&app, "tracy.brittcool@kanbrick.com", "pw-l5").await;
    let (status, _) = invoke(&app, &l5, "does-not-exist", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
