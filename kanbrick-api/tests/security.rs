//! #48 — security audit suite (HTTP vectors). Probes clearance escalation, JWT
//! manipulation (forged, tampered, expired), and Cypher injection through the
//! guest endpoint. (Guest sandbox-escape is covered structurally by the mesh's
//! locked-down WASIp1 context + `resource_limits` tests; per-query audit by
//! `kanbrick-auth`'s `guarded`/`data_integrity` tests.)

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Duration;
use http_body_util::BodyExt;
use kanbrick_api::{router, AppState};
use kanbrick_auth::{JwtAuthenticator, LoginService};
use kanbrick_core::{ClearanceLevel, FirmContext};
use kanbrick_store::{seed, Migrator, Store};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const SECRET: &[u8] = b"security-suite-secret";

/// Seed a system signed with [`SECRET`], with an L2 and L5 login provisioned.
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
        svc.set_password("tracy.brittcool@kanbrick.com", "pw5")
            .unwrap();
        svc.set_password("elena.ruiz@kanbrick.com", "pw2").unwrap();
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

async fn get_with_token(app: &axum::Router, uri: &str, token: &str) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn post_guest(
    app: &axum::Router,
    guest: &str,
    token: &str,
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
    (status, body(resp).await)
}

fn ctx(email: &str, clearance: ClearanceLevel) -> FirmContext {
    FirmContext::new(Uuid::new_v4(), email, clearance)
}

#[tokio::test]
async fn forged_clearance_token_is_rejected() {
    let (_d, app) = app();
    // An attacker mints a token claiming L5 — but signs it with the WRONG secret.
    // The signature check rejects it: you cannot escalate by forging claims.
    let forger = JwtAuthenticator::new(b"attacker-secret", Duration::hours(1));
    let forged = forger
        .issue(&ctx("elena.ruiz@kanbrick.com", ClearanceLevel::L5))
        .unwrap();
    assert_eq!(
        get_with_token(&app, "/me", &forged).await,
        StatusCode::UNAUTHORIZED
    );
    let (status, _) = post_guest(&app, "compliance", &forged, json!({"check": "all"})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_valid_low_clearance_token_cannot_reach_higher_guests() {
    let (_d, app) = app();
    // No forgery — a genuine L2 token, but the clearance gate forbids the L3/L4 guests.
    let l2 = login(&app, "elena.ruiz@kanbrick.com", "pw2").await;
    assert_eq!(
        post_guest(&app, "valuation", &l2, json!({"company_id": "JMTS"}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post_guest(&app, "compliance", &l2, json!({"check": "all"}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // And the coarse /admin gate too.
    assert_eq!(
        get_with_token(&app, "/admin", &l2).await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn expired_token_is_rejected() {
    let (_d, app) = app();
    // Mint an already-expired token with the *correct* secret (negative TTL).
    let expired_issuer = JwtAuthenticator::new(SECRET, Duration::seconds(-10));
    let expired = expired_issuer
        .issue(&ctx("tracy.brittcool@kanbrick.com", ClearanceLevel::L5))
        .unwrap();
    assert_eq!(
        get_with_token(&app, "/me", &expired).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn tampered_token_is_rejected() {
    let (_d, app) = app();
    let token = login(&app, "tracy.brittcool@kanbrick.com", "pw5").await;
    // Flip a byte in the signature segment.
    let mut bytes = token.into_bytes();
    *bytes.last_mut().unwrap() ^= 0x01;
    let tampered = String::from_utf8(bytes).unwrap();
    assert_eq!(
        get_with_token(&app, "/me", &tampered).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn cypher_injection_through_a_guest_payload_is_harmless() {
    let (_d, app) = app();
    let l5 = login(&app, "tracy.brittcool@kanbrick.com", "pw5").await;
    // A classic injection string as the company_id. It is *bound* as a parameter
    // (never interpolated), so it matches no company — no leak, no crash, no
    // escalation. The guest reports the company as not found.
    let evil = "JMTS\" OR \"1\"=\"1";
    let (status, body) = post_guest(&app, "valuation", &l5, json!({"company_id": evil})).await;
    // Either a structured 200 error payload or a 404 — never a 500 / data dump.
    assert!(status == StatusCode::OK || status == StatusCode::NOT_FOUND);
    if status == StatusCode::OK {
        assert!(
            body.get("error").is_some(),
            "no valuation for an injected id"
        );
    }
    // The real company still values normally (the system is intact).
    let (ok, report) = post_guest(&app, "valuation", &l5, json!({"company_id": "JMTS"})).await;
    assert_eq!(ok, StatusCode::OK);
    assert!(report.get("enterprise_value").is_some());
}
