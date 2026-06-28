//! P11.2 — the ScopeGrants grant-lifecycle HTTP surface (`/me/scope-requests`,
//! `/me/scopes`). Asserts the dual-gate end to end over HTTP: a request is submitted
//! as the host-authoritative caller, a sub-clearance caller cannot approve, an
//! eligible grantor (in the requester's management chain) and an L5 cofounder both
//! can, an approved scope lists as active, and the grantor can revoke it.
//!
//! Identities come from the firm seed's real org chart: elena (L2) requests; peter
//! (CSO, L4) is in her management chain → eligible grantor; tracy (CEO, L5) is the
//! cofounder override. The same chain the `kanbrick-discovery` grant unit tests use.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Duration;
use http_body_util::BodyExt;
use kanbrick_api::{router, AppState};
use kanbrick_auth::{JwtAuthenticator, LoginService};
use kanbrick_store::{Migrator, Store};
use serde_json::{json, Value};
use tower::ServiceExt;

const SECRET: &[u8] = b"grants-suite-secret";

const ELENA: &str = "elena.ruiz@kanbrick.com"; // L2 — requester
const TYLER: &str = "tyler.begemann@kanbrick.com"; // L3 — neither requester nor a grantor
const PETER: &str = "peter.nash@kanbrick.com"; // L4 — in Elena's chain → eligible grantor
const TRACY: &str = "tracy.brittcool@kanbrick.com"; // L5 (CEO) — eligible regardless of chain

fn app() -> (tempfile::TempDir, axum::Router) {
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
        svc.set_password(ELENA, "pw2").unwrap();
        svc.set_password(TYLER, "pw3").unwrap();
        svc.set_password(PETER, "pw4").unwrap();
        svc.set_password(TRACY, "pw5").unwrap();
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

async fn post(
    app: &axum::Router,
    uri: &str,
    token: Option<&str>,
    payload: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
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
    (status, body(resp).await)
}

async fn get(app: &axum::Router, uri: &str, token: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, body(resp).await)
}

/// Submit Elena's standard scope request (JMTS + the segment lead Tyler), returning
/// the created request JSON.
async fn request_jmts(app: &axum::Router, token: &str) -> Value {
    post(
        app,
        "/me/scope-requests",
        Some(token),
        json!({
            "project": "valuation-jmts",
            "persons": ["tyler.begemann@kanbrick.com"],
            "companies": ["JMTS"],
            "justification": "Need JMTS + the segment lead for the valuation."
        }),
    )
    .await
    .1
}

#[tokio::test]
async fn unauthenticated_request_is_rejected() {
    let (_d, app) = app();
    let (status, _) = post(&app, "/me/scope-requests", None, json!({ "project": "x" })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn request_then_read_round_trips() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;

    let req = request_jmts(&app, &elena).await;
    assert_eq!(req["status"], "requested");
    assert_eq!(req["requester"], ELENA);
    assert_eq!(req["companies"], json!(["JMTS"]));
    let id = req["id"].as_str().unwrap();

    // The requester can read it back.
    let (status, read) = get(&app, &format!("/me/scope-requests/{id}"), &elena).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read["status"], "requested");
}

#[tokio::test]
async fn a_sub_clearance_caller_cannot_approve() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let id = request_jmts(&app, &elena).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Elena (L2) is below the L4 grantor floor.
    let (status, _) = post(
        &app,
        &format!("/me/scope-requests/{id}/approve"),
        Some(&elena),
        json!({ "ttl_days": 30 }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_eligible_grantor_approves_and_the_scope_lists_active() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;
    let id = request_jmts(&app, &elena).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Peter (L4, in Elena's chain) approves.
    let (status, granted) = post(
        &app,
        &format!("/me/scope-requests/{id}/approve"),
        Some(&peter),
        json!({ "ttl_days": 30 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(granted["status"], "active");
    assert_eq!(granted["granted_by"], PETER);
    assert_eq!(granted["requester"], ELENA);

    // The request is now granted, and the scope lists as active for Elena.
    let (_s, read) = get(&app, &format!("/me/scope-requests/{id}"), &elena).await;
    assert_eq!(read["status"], "granted");

    let (status, scopes) = get(&app, "/me/scopes?project=valuation-jmts", &elena).await;
    assert_eq!(status, StatusCode::OK);
    let arr = scopes.as_array().unwrap();
    assert_eq!(arr.len(), 1, "one active scope");
    assert_eq!(arr[0]["status"], "active");
}

#[tokio::test]
async fn an_l5_cofounder_can_approve() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let tracy = login(&app, TRACY, "pw5").await;
    let id = request_jmts(&app, &elena).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Tracy (CEO, L5) is an eligible grantor regardless of the management chain.
    let (status, granted) = post(
        &app,
        &format!("/me/scope-requests/{id}/approve"),
        Some(&tracy),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(granted["status"], "active");
    assert_eq!(granted["granted_by"], TRACY);
}

#[tokio::test]
async fn the_grantor_can_revoke_an_active_scope() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;
    let id = request_jmts(&app, &elena).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let granted = post(
        &app,
        &format!("/me/scope-requests/{id}/approve"),
        Some(&peter),
        json!({ "ttl_days": 30 }),
    )
    .await
    .1;
    let scope_id = granted["id"].as_str().unwrap().to_string();

    // The granting grantor revokes it.
    let (status, _) = post(
        &app,
        &format!("/me/scopes/{scope_id}/revoke"),
        Some(&peter),
        json!({ "reason": "no longer needed" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // It is no longer active for Elena.
    let (_s, scopes) = get(&app, "/me/scopes?project=valuation-jmts", &elena).await;
    assert!(
        scopes.as_array().unwrap().is_empty(),
        "a revoked scope is not active"
    );
}

#[tokio::test]
async fn an_eligible_grantor_can_deny() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;
    let id = request_jmts(&app, &elena).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _) = post(
        &app,
        &format!("/me/scope-requests/{id}/deny"),
        Some(&peter),
        json!({ "reason": "out of scope for this engagement" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // The request reads back as denied.
    let (_s, read) = get(&app, &format!("/me/scope-requests/{id}"), &elena).await;
    assert_eq!(read["status"], "denied");
}

#[tokio::test]
async fn a_sub_clearance_caller_cannot_deny() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let id = request_jmts(&app, &elena).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _) = post(
        &app,
        &format!("/me/scope-requests/{id}/deny"),
        Some(&elena),
        json!({ "reason": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_non_grantor_cannot_revoke() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;
    let id = request_jmts(&app, &elena).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let granted = post(
        &app,
        &format!("/me/scope-requests/{id}/approve"),
        Some(&peter),
        json!({ "ttl_days": 30 }),
    )
    .await
    .1;
    let scope_id = granted["id"].as_str().unwrap().to_string();

    // Elena is the grantee but neither the granting grantor nor L5 → cannot revoke.
    let (status, _) = post(
        &app,
        &format!("/me/scopes/{scope_id}/revoke"),
        Some(&elena),
        json!({ "reason": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The scope survives the rejected revoke.
    let (_s, scopes) = get(&app, "/me/scopes?project=valuation-jmts", &elena).await;
    assert_eq!(
        scopes.as_array().unwrap().len(),
        1,
        "still active after a rejected revoke"
    );
}

#[tokio::test]
async fn a_non_owner_below_grantor_clearance_cannot_read_a_request() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let tyler = login(&app, TYLER, "pw3").await;
    let id = request_jmts(&app, &elena).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Tyler (L3) is neither the requester nor an L4+ reviewer.
    let (status, _) = get(&app, &format!("/me/scope-requests/{id}"), &tyler).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
