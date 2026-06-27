//! P10.1 (#113) + P10.2 (#114) — `/me/messenger` over HTTP. Asserts the acceptance
//! criteria end-to-end: unauthenticated is `401`, a send is persisted with a
//! host-authoritative `actor` (never the body) and emitted on the bus, the log
//! replays prior sends from the **durable store** honoring `kind` + `limit`, group
//! scope round-trips, every send is audited under the caller's identity, and each
//! send writes a durable `(:MessengerMessage)` node.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Duration;
use http_body_util::BodyExt;
use kanbrick_api::{router, AppState};
use kanbrick_auth::{AuditLog, JwtAuthenticator, LoginService};
use kanbrick_store::{count_messages, Migrator, Store};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

const SECRET: &[u8] = b"messenger-suite-secret";

const ELENA: &str = "elena.ruiz@kanbrick.com";
const TRACY: &str = "tracy.brittcool@kanbrick.com";

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
        svc.set_password(TRACY, "pw5").unwrap();
        svc.set_password(ELENA, "pw2").unwrap();
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
        send(&app, "GET", "/me/messenger/log", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(
            &app,
            "POST",
            "/me/messenger/send",
            None,
            Some(json!({ "text": "hello" })),
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn send_then_log_round_trip_uses_host_authoritative_actor() {
    let (_d, app, _store) = app();
    let token = login(&app, ELENA, "pw2").await;

    // Send a public message — scope omitted, so it defaults to public. The body
    // carries no `actor`; the server stamps the caller's identity.
    let (status, sent) = send(
        &app,
        "POST",
        "/me/messenger/send",
        Some(&token),
        Some(json!({ "text": "hello team" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sent["actor"], ELENA);
    assert_eq!(sent["text"], "hello team");
    assert_eq!(sent["scope"], json!({ "kind": "public" }));

    // Replay — the message comes back from the durable store.
    let (status, log) = send(&app, "GET", "/me/messenger/log", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    let arr = log.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["actor"], ELENA);
    assert_eq!(arr[0]["text"], "hello team");
    assert_eq!(arr[0]["scope"], json!({ "kind": "public" }));
}

#[tokio::test]
async fn actor_cannot_be_spoofed_via_the_body() {
    let (_d, app, _store) = app();
    let token = login(&app, ELENA, "pw2").await;

    // A client tries to post as someone else — the `actor` field is ignored.
    let (status, sent) = send(
        &app,
        "POST",
        "/me/messenger/send",
        Some(&token),
        Some(json!({ "text": "not me", "actor": "ceo@kanbrick.com" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sent["actor"], ELENA);
}

#[tokio::test]
async fn group_scope_round_trips() {
    let (_d, app, _store) = app();
    let token = login(&app, ELENA, "pw2").await;

    let (status, sent) = send(
        &app,
        "POST",
        "/me/messenger/send",
        Some(&token),
        Some(json!({
            "text": "standup at 10",
            "scope": { "kind": "group", "name": "engineering" }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        sent["scope"],
        json!({ "kind": "group", "name": "engineering" })
    );

    let (_, log) = send(&app, "GET", "/me/messenger/log", Some(&token), None).await;
    let arr = log.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(
        arr[0]["scope"],
        json!({ "kind": "group", "name": "engineering" })
    );
}

#[tokio::test]
async fn log_honors_limit_and_kind_filter() {
    let (_d, app, _store) = app();
    let token = login(&app, ELENA, "pw2").await;

    for text in ["first", "second", "third"] {
        send(
            &app,
            "POST",
            "/me/messenger/send",
            Some(&token),
            Some(json!({ "text": text })),
        )
        .await;
    }

    // limit=2 returns the two most recent, oldest→newest.
    let (status, log) = send(&app, "GET", "/me/messenger/log?limit=2", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    let arr = log.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["text"], "second");
    assert_eq!(arr[1]["text"], "third");

    // An unrelated kind replays nothing.
    let (_, other) = send(
        &app,
        "GET",
        "/me/messenger/log?kind=valuation.completed",
        Some(&token),
        None,
    )
    .await;
    assert!(other.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn messages_are_visible_across_users_on_the_shared_bus() {
    let (_d, app, _store) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let tracy = login(&app, TRACY, "pw5").await;

    send(
        &app,
        "POST",
        "/me/messenger/send",
        Some(&elena),
        Some(json!({ "text": "all hands" })),
    )
    .await;

    // Tracy reads the same firm-wide bus log and sees Elena's message.
    let (_, log) = send(&app, "GET", "/me/messenger/log", Some(&tracy), None).await;
    let arr = log.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["actor"], ELENA);
    assert_eq!(arr[0]["text"], "all hands");
}

#[tokio::test]
async fn every_send_persists_a_durable_messenger_message_node() {
    let (_d, app, store) = app();
    let token = login(&app, ELENA, "pw2").await;

    assert_eq!(count_messages(&store).unwrap(), 0);
    for text in ["durable one", "durable two"] {
        send(
            &app,
            "POST",
            "/me/messenger/send",
            Some(&token),
            Some(json!({ "text": text })),
        )
        .await;
    }
    // Each send writes one durable (:MessengerMessage) node to the store.
    assert_eq!(count_messages(&store).unwrap(), 2);

    // ...and the log endpoint replays them from that durable store, oldest→newest.
    let (_, log) = send(&app, "GET", "/me/messenger/log", Some(&token), None).await;
    let arr = log.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["text"], "durable one");
    assert_eq!(arr[1]["text"], "durable two");
}

#[tokio::test]
async fn every_send_is_audited_under_the_caller() {
    let (_d, app, store) = app();
    let token = login(&app, ELENA, "pw2").await;
    let uid = user_id_of(&token);

    let before = AuditLog::new(&store).count_for_user(uid).unwrap();
    for text in ["one", "two"] {
        send(
            &app,
            "POST",
            "/me/messenger/send",
            Some(&token),
            Some(json!({ "text": text })),
        )
        .await;
    }
    // Each send records one audit entry under Elena's identity.
    let after = AuditLog::new(&store).count_for_user(uid).unwrap();
    assert_eq!(after - before, 2);
}
