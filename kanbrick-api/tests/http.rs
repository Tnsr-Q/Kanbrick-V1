//! Phase 2 HTTP checkpoints (issues #15, #16): login → JWT, invalid JWT → 401,
//! clearance gate → 403.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Duration;
use http_body_util::BodyExt;
use kanbrick_api::{router, AppState};
use kanbrick_auth::{JwtAuthenticator, LoginService};
use kanbrick_store::{Migrator, Store};
use tower::ServiceExt; // for `oneshot`

/// Build a seeded app with two provisioned logins (an L5 CEO and an L2 analyst).
///
/// Returns the `TempDir` alongside the router so the store's backing files live
/// for the duration of the test.
fn app_with_logins() -> (tempfile::TempDir, axum::Router) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let seed = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../seed/kanbrick_seed_data.cypher"
    ))
    .unwrap();
    Migrator::firm(seed).run(&store).unwrap();

    let jwt = JwtAuthenticator::new(b"test-secret", Duration::hours(1));
    {
        let svc = LoginService::new(&store, &jwt);
        svc.set_password("tracy.brittcool@kanbrick.com", "ceo-pw")
            .unwrap();
        svc.set_password("elena.ruiz@kanbrick.com", "analyst-pw")
            .unwrap();
    }
    (dir, router(AppState::new(store, jwt)))
}

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

async fn login(app: &axum::Router, email: &str, password: &str) -> axum::response::Response {
    let body = serde_json::json!({ "email": email, "password": password }).to_string();
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/login")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn login_returns_jwt_then_me_returns_clearance() {
    let (_dir, app) = app_with_logins();

    let resp = login(&app, "tracy.brittcool@kanbrick.com", "ceo-pw").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let token = json["token"].as_str().unwrap().to_string();

    // The token authenticates GET /me and reports L5.
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
    let me: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(me["clearance"], "L5");
    assert_eq!(me["email"], "tracy.brittcool@kanbrick.com");
}

#[tokio::test]
async fn bad_password_is_401() {
    let (_dir, app) = app_with_logins();
    let resp = login(&app, "tracy.brittcool@kanbrick.com", "wrong").await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let json: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(json["error"]["kind"], "unauthorized");
}

#[tokio::test]
async fn missing_and_invalid_tokens_are_401() {
    let (_dir, app) = app_with_logins();

    // No Authorization header.
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Garbage token.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/me")
                .header("Authorization", "Bearer not.a.jwt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn clearance_gate_forbids_l2_but_allows_l5() {
    let (_dir, app) = app_with_logins();

    // L2 analyst is forbidden from the L4-gated /admin route.
    let resp = login(&app, "elena.ruiz@kanbrick.com", "analyst-pw").await;
    let json: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let l2_token = json["token"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin")
                .header("Authorization", format!("Bearer {l2_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let json: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(json["error"]["kind"], "forbidden");

    // The L5 CEO is admitted.
    let resp = login(&app, "tracy.brittcool@kanbrick.com", "ceo-pw").await;
    let json: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let l5_token = json["token"].as_str().unwrap().to_string();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/admin")
                .header("Authorization", format!("Bearer {l5_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
