//! P9.3 (#103) — `/me/provider-keys` custody routes over HTTP. Asserts the
//! acceptance criteria end-to-end against the in-memory backend: unauthenticated
//! is `401`, reads are metadata-only, keys are namespaced per `user_id` (cross-user
//! read/delete is impossible), and every action is audited.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Duration;
use http_body_util::BodyExt;
use kanbrick_api::{router, AppState};
use kanbrick_auth::{AuditLog, JwtAuthenticator, LoginService};
use kanbrick_store::{Migrator, Store};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

const SECRET: &[u8] = b"provider-keys-suite-secret";

/// Seed a system with an L2 (`elena`) and L5 (`tracy`) login, returning the router
/// plus a handle to the store so the test can read the audit log.
fn app() -> (tempfile::TempDir, axum::Router, Arc<Store>) {
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
        svc.set_password("tracy.brittcool@kanbrick.com", "pw5")
            .unwrap();
        svc.set_password("elena.ruiz@kanbrick.com", "pw2").unwrap();
    }
    let state = AppState::new(store, jwt).unwrap();
    let store_handle = state.store.clone();
    (dir, router(state), store_handle)
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

/// Send a request, optionally authenticated and/or with a JSON body.
async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    payload: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        builder = builder.header("Authorization", format!("Bearer {t}"));
    }
    let body_bytes = match &payload {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = app
        .clone()
        .oneshot(builder.body(body_bytes).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    (status, body(resp).await)
}

/// Decode a token (signed with [`SECRET`]) back to its `user_id`.
fn user_id_of(token: &str) -> Uuid {
    JwtAuthenticator::new(SECRET, Duration::hours(1))
        .validate(token)
        .unwrap()
        .user_id
}

#[tokio::test]
async fn unauthenticated_requests_are_rejected() {
    let (_d, app, _store) = app();
    assert_eq!(
        send(&app, "GET", "/me/provider-keys", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(
            &app,
            "POST",
            "/me/provider-keys",
            None,
            Some(json!({"provider": "openai", "label": "x", "secret": "sk-1"})),
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn create_list_delete_round_trip_returns_metadata_only() {
    let (_d, app, _store) = app();
    let token = login(&app, "elena.ruiz@kanbrick.com", "pw2").await;

    // Create — returns metadata, never the secret.
    let (status, created) = send(
        &app,
        "POST",
        "/me/provider-keys",
        Some(&token),
        Some(
            json!({"provider": "openai", "label": "personal-openai", "secret": "sk-super-secret"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(created["provider"], "openai");
    assert_eq!(created["label"], "personal-openai");
    assert!(created.get("secret").is_none());
    let id = created["id"].as_str().unwrap().to_string();

    // List — metadata only; the secret value appears nowhere.
    let (status, listed) = send(&app, "GET", "/me/provider-keys", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    let arr = listed.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], id);
    assert!(!listed.to_string().contains("sk-super-secret"));

    // Delete — 204, then the list is empty.
    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/me/provider-keys/{id}"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, after) = send(&app, "GET", "/me/provider-keys", Some(&token), None).await;
    assert!(after.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn keys_are_namespaced_per_user_no_cross_user_access() {
    let (_d, app, _store) = app();
    let elena = login(&app, "elena.ruiz@kanbrick.com", "pw2").await;
    let tracy = login(&app, "tracy.brittcool@kanbrick.com", "pw5").await;

    // Elena stores a key.
    let (_, created) = send(
        &app,
        "POST",
        "/me/provider-keys",
        Some(&elena),
        Some(json!({"provider": "anthropic", "label": "elena-key", "secret": "elena-secret"})),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    // Tracy is a different user_id namespace: she sees none of Elena's keys...
    let (_, tracy_list) = send(&app, "GET", "/me/provider-keys", Some(&tracy), None).await;
    assert!(tracy_list.as_array().unwrap().is_empty());

    // ...and deleting Elena's key id from Tracy's namespace is a 404, not a cross-user delete.
    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/me/provider-keys/{id}"),
        Some(&tracy),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Elena's key is untouched.
    let (_, elena_list) = send(&app, "GET", "/me/provider-keys", Some(&elena), None).await;
    assert_eq!(elena_list.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn every_action_is_audited_under_the_caller() {
    let (_d, app, store) = app();
    let token = login(&app, "elena.ruiz@kanbrick.com", "pw2").await;
    let uid = user_id_of(&token);

    let before = AuditLog::new(&store).count_for_user(uid).unwrap();
    let (_, created) = send(
        &app,
        "POST",
        "/me/provider-keys",
        Some(&token),
        Some(json!({"provider": "cerebras", "label": "k", "secret": "s"})),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    send(&app, "GET", "/me/provider-keys", Some(&token), None).await;
    send(
        &app,
        "DELETE",
        &format!("/me/provider-keys/{id}"),
        Some(&token),
        None,
    )
    .await;

    // create + list + delete each recorded one audit entry under Elena's identity.
    let after = AuditLog::new(&store).count_for_user(uid).unwrap();
    assert_eq!(after - before, 3);
}
