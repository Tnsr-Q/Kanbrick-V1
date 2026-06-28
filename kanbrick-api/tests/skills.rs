//! P11.2b — the skill-registry ⇄ grant bridge (`/me/skills`,
//! `/me/scopes/{id}/skills`). Asserts the catalogue and bridge end to end over
//! HTTP: publishing host-stamps the author and is L3-gated, malformed `SKILL.md`
//! is a `400`, the catalogue/history list, and a scope *owner* can bind a published
//! edition onto an approved scope (without a run-time clearance re-check) while a
//! non-owner cannot.
//!
//! The approved scope is built by chaining the P11.2 grant routes: elena (L2)
//! requests, peter (CSO, L4, in her chain) approves — the same org chart the grant
//! suite uses.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Duration;
use http_body_util::BodyExt;
use kanbrick_api::{router, AppState};
use kanbrick_auth::{JwtAuthenticator, LoginService};
use kanbrick_store::{Migrator, Store};
use serde_json::{json, Value};
use tower::ServiceExt;

const SECRET: &[u8] = b"skills-suite-secret";

const ELENA: &str = "elena.ruiz@kanbrick.com"; // L2 — requester / scope owner
const TYLER: &str = "tyler.begemann@kanbrick.com"; // L3 — can publish; not an owner
const PETER: &str = "peter.nash@kanbrick.com"; // L4 — in Elena's chain → eligible grantor
const TRACY: &str = "tracy.brittcool@kanbrick.com"; // L5 (CEO) — cofounder override

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

/// A canonical `SKILL.md` for the valuation guest at `clearance`.
fn skill_md(name: &str, version: &str, clearance: &str) -> String {
    format!(
        "---\nname: {name}\nversion: {version}\nguest: valuation\nclearance: {clearance}\n\
         description: Model a deal from financials\n---\n\n# {name}\n\nRun the valuation guest.\n"
    )
}

/// Publish `name@version` (valuation guest, `clearance`) as `token`, returning the
/// stored edition JSON.
async fn publish(
    app: &axum::Router,
    token: &str,
    name: &str,
    version: &str,
    clearance: &str,
) -> (StatusCode, Value) {
    post(
        app,
        "/me/skills",
        Some(token),
        json!({ "skill_md": skill_md(name, version, clearance) }),
    )
    .await
}

/// Chain the P11.2 grant routes into an approved scope owned by Elena: she requests
/// JMTS, Peter (L4, in chain) approves. Returns the granted scope id.
async fn approved_scope(app: &axum::Router, elena: &str, peter: &str) -> String {
    let req = post(
        app,
        "/me/scope-requests",
        Some(elena),
        json!({
            "project": "valuation-jmts",
            "persons": ["tyler.begemann@kanbrick.com"],
            "companies": ["JMTS"],
            "justification": "Need JMTS for the valuation."
        }),
    )
    .await
    .1;
    let id = req["id"].as_str().unwrap().to_string();
    let granted = post(
        app,
        &format!("/me/scope-requests/{id}/approve"),
        Some(peter),
        json!({ "ttl_days": 30 }),
    )
    .await
    .1;
    granted["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn unauthenticated_publish_is_rejected() {
    let (_d, app) = app();
    let (status, _) = post(
        &app,
        "/me/skills",
        None,
        json!({ "skill_md": skill_md("x", "1.0.0", "L3") }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn publishing_below_l3_is_forbidden() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await; // L2
    let (status, _) = publish(&app, &elena, "deal-modeling", "1.0.0", "L3").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn malformed_skill_md_is_a_400() {
    let (_d, app) = app();
    let tyler = login(&app, TYLER, "pw3").await; // L3 — clears the publish gate
    let (status, err) = post(
        &app,
        "/me/skills",
        Some(&tyler),
        json!({ "skill_md": "no frontmatter fence here\njust prose" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["kind"], "invalid_skill_md");
}

#[tokio::test]
async fn publish_host_stamps_the_author_and_ignores_a_body_source() {
    let (_d, app) = app();
    let peter = login(&app, PETER, "pw4").await;
    // Even with an attacker-supplied `source` in the body, the author is the
    // host-authoritative caller; the extra field is ignored by the body shape.
    let (status, published) = post(
        &app,
        "/me/skills",
        Some(&peter),
        json!({
            "skill_md": skill_md("deal-modeling", "1.0.0", "L3"),
            "source": "attacker@evil.com",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(published["source"], PETER, "author is host-stamped");
    assert_eq!(published["skill_name"], "deal-modeling");
    assert_eq!(published["min_clearance"], "L3");
    assert!(
        published["seq"].is_number(),
        "carries the store-assigned seq"
    );
}

#[tokio::test]
async fn publish_then_browse_latest_and_list_history() {
    let (_d, app) = app();
    let peter = login(&app, PETER, "pw4").await;
    publish(&app, &peter, "deal-modeling", "1.0.0", "L3").await;
    let (status, _) = publish(&app, &peter, "deal-modeling", "1.1.0", "L4").await;
    assert_eq!(status, StatusCode::OK);

    // The catalogue shows the latest edition, one row per skill.
    let (status, catalogue) = get(&app, "/me/skills", &peter).await;
    assert_eq!(status, StatusCode::OK);
    let rows = catalogue.as_array().unwrap();
    assert_eq!(rows.len(), 1, "one row per skill name");
    assert_eq!(rows[0]["version"], "1.1.0", "latest edition represents it");

    // History lists every edition, oldest→newest.
    let (status, history) = get(&app, "/me/skills/deal-modeling", &peter).await;
    assert_eq!(status, StatusCode::OK);
    let versions: Vec<&str> = history
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["version"].as_str().unwrap())
        .collect();
    assert_eq!(versions, ["1.0.0", "1.1.0"]);
}

#[tokio::test]
async fn an_owner_binds_a_published_edition_onto_their_scope() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await; // L2 — owner
    let peter = login(&app, PETER, "pw4").await; // L4 — grantor + publisher

    let scope_id = approved_scope(&app, &elena, &peter).await;
    publish(&app, &peter, "deal-modeling", "1.0.0", "L3").await;

    // Elena owns the scope, so she may bind — even though the skill needs L3 to
    // *run* and she is only L2. Define ≠ run: no clearance re-check here.
    let (status, skill) = post(
        &app,
        &format!("/me/scopes/{scope_id}/skills"),
        Some(&elena),
        json!({ "skill_name": "deal-modeling" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(skill["name"], "deal-modeling");
    assert_eq!(skill["guest"], "valuation");
    assert_eq!(skill["scope_id"], scope_id);
    assert_eq!(
        skill["required_clearance"], "L3",
        "the skill carries the edition's clearance floor"
    );

    // The owner can read the bound skills back.
    let (status, bound) = get(&app, &format!("/me/scopes/{scope_id}/skills"), &elena).await;
    assert_eq!(status, StatusCode::OK);
    let arr = bound.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "deal-modeling");
}

#[tokio::test]
async fn a_non_owner_cannot_bind_a_skill_onto_a_scope() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;
    let tyler = login(&app, TYLER, "pw3").await; // L3, but not the owner nor L5

    let scope_id = approved_scope(&app, &elena, &peter).await;
    publish(&app, &peter, "deal-modeling", "1.0.0", "L3").await;

    let (status, _) = post(
        &app,
        &format!("/me/scopes/{scope_id}/skills"),
        Some(&tyler),
        json!({ "skill_name": "deal-modeling" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Nothing was bound.
    let (_s, bound) = get(&app, &format!("/me/scopes/{scope_id}/skills"), &elena).await;
    assert!(bound.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn an_l5_cofounder_can_bind_onto_someone_elses_scope() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;
    let tracy = login(&app, TRACY, "pw5").await; // L5 — override

    let scope_id = approved_scope(&app, &elena, &peter).await;
    publish(&app, &peter, "deal-modeling", "1.0.0", "L3").await;

    let (status, _) = post(
        &app,
        &format!("/me/scopes/{scope_id}/skills"),
        Some(&tracy),
        json!({ "skill_name": "deal-modeling" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn binding_an_unpublished_skill_is_404() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;
    let scope_id = approved_scope(&app, &elena, &peter).await;

    let (status, err) = post(
        &app,
        &format!("/me/scopes/{scope_id}/skills"),
        Some(&elena),
        json!({ "skill_name": "ghost-skill" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(err["error"]["kind"], "not_found");
}

#[tokio::test]
async fn binding_onto_an_unknown_scope_is_404() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;
    publish(&app, &peter, "deal-modeling", "1.0.0", "L3").await;

    let (status, _) = post(
        &app,
        "/me/scopes/00000000-0000-0000-0000-000000000000/skills",
        Some(&elena),
        json!({ "skill_name": "deal-modeling" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_l4_reviewer_can_read_a_scopes_skills() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await; // L4 — not the owner
    let scope_id = approved_scope(&app, &elena, &peter).await;
    publish(&app, &peter, "deal-modeling", "1.0.0", "L3").await;
    post(
        &app,
        &format!("/me/scopes/{scope_id}/skills"),
        Some(&elena),
        json!({ "skill_name": "deal-modeling" }),
    )
    .await;

    // Peter is not the grantee, but L4 clears the inspect floor.
    let (status, bound) = get(&app, &format!("/me/scopes/{scope_id}/skills"), &peter).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bound.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn binding_a_nonexistent_pinned_version_is_404() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;
    let scope_id = approved_scope(&app, &elena, &peter).await;
    publish(&app, &peter, "deal-modeling", "1.0.0", "L3").await;

    // The skill exists, but not at the pinned version.
    let (status, _) = post(
        &app,
        &format!("/me/scopes/{scope_id}/skills"),
        Some(&elena),
        json!({ "skill_name": "deal-modeling", "version": "9.9.9" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_owner_can_bind_a_specific_published_version() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;
    let scope_id = approved_scope(&app, &elena, &peter).await;

    // Two editions: the older needs L2, the newer L4.
    publish(&app, &peter, "deal-modeling", "1.0.0", "L2").await;
    publish(&app, &peter, "deal-modeling", "2.0.0", "L4").await;

    // Pin the older edition explicitly; its clearance floor must be the bound one.
    let (status, skill) = post(
        &app,
        &format!("/me/scopes/{scope_id}/skills"),
        Some(&elena),
        json!({ "skill_name": "deal-modeling", "version": "1.0.0" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        skill["required_clearance"], "L2",
        "the pinned edition's clearance, not the latest"
    );
}
