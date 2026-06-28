//! P11.3 — the loop run engine (`/me/loops`, `/me/loops/{id}/run`,
//! `/me/loops/runs/{id}`). Asserts the walking skeleton end to end over HTTP: a loop
//! is authored, **runs** through the real run gate (`authorize_skill`), and its
//! per-step status is observable live — plus the gate (an unauthorized step is
//! denied and fails the run) and ownership (only the owner may run/read a loop).
//!
//! The happy path binds a skill backed by the L1 `reporting` guest (runs for any
//! tier) onto a P11.2-approved scope, so a real guest invocation Completes. The
//! approved scope and the bound skill are built by chaining the P11.2/P11.2b routes:
//! elena (L2) requests → peter (L4) approves → peter publishes → elena binds.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Duration;
use http_body_util::BodyExt;
use kanbrick_api::{router, AppState};
use kanbrick_auth::{JwtAuthenticator, LoginService};
use kanbrick_store::{seed, Migrator, Store};
use serde_json::{json, Value};
use tower::ServiceExt;

const SECRET: &[u8] = b"loops-suite-secret";

const ELENA: &str = "elena.ruiz@kanbrick.com"; // L2 — loop owner / scope grantee
const PETER: &str = "peter.nash@kanbrick.com"; // L4 — grantor + skill publisher

fn app() -> (tempfile::TempDir, axum::Router) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let firm = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../seed/kanbrick_seed_data.cypher"
    ))
    .unwrap();
    Migrator::firm(firm).run(&store).unwrap();
    // The reporting guest reads financials for detail; load them so a real run
    // Completes (mirrors the e2e suite's seeded_app).
    let financials = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../seed/kanbrick_financials.cypher"
    ))
    .unwrap();
    seed::load_str(&store, &financials).unwrap();

    let jwt = JwtAuthenticator::new(SECRET, Duration::hours(1));
    {
        let svc = LoginService::new(&store, &jwt);
        svc.set_password(ELENA, "pw2").unwrap();
        svc.set_password(PETER, "pw4").unwrap();
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

fn skill_md(name: &str, guest: &str, clearance: &str) -> String {
    format!(
        "---\nname: {name}\nversion: 1.0.0\nguest: {guest}\nclearance: {clearance}\n\
         description: a loop step\n---\n\n# {name}\n\nRun it.\n"
    )
}

/// elena requests JMTS, peter (L4, in chain) approves → returns the granted scope id.
async fn approved_scope(app: &axum::Router, elena: &str, peter: &str) -> String {
    let req = post(
        app,
        "/me/scope-requests",
        Some(elena),
        json!({ "project": "valuation-jmts", "companies": ["JMTS"], "justification": "j" }),
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

/// Publish `name`(guest, clearance) as peter, then bind it onto `scope_id` as elena.
async fn publish_and_bind(
    app: &axum::Router,
    elena: &str,
    peter: &str,
    scope_id: &str,
    name: &str,
    guest: &str,
    clearance: &str,
) {
    let (s, _) = post(
        app,
        "/me/skills",
        Some(peter),
        json!({ "skill_md": skill_md(name, guest, clearance) }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "publish failed");
    let (s, _) = post(
        app,
        &format!("/me/scopes/{scope_id}/skills"),
        Some(elena),
        json!({ "skill_name": name }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "bind failed");
}

/// Poll a run until it leaves `running`, or give up after ~6s.
async fn poll_run(app: &axum::Router, token: &str, run_id: &str) -> Value {
    for _ in 0..120 {
        let (status, run) = get(app, &format!("/me/loops/runs/{run_id}"), token).await;
        assert_eq!(status, StatusCode::OK);
        if run["status"] != "running" {
            return run;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("run {run_id} never reached a terminal status");
}

#[tokio::test]
async fn unauthenticated_loop_create_is_rejected() {
    let (_d, app) = app();
    let (status, _) = post(&app, "/me/loops", None, json!({ "name": "x" })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_then_read_a_loop_round_trips() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let (status, created) = post(
        &app,
        "/me/loops",
        Some(&elena),
        json!({
            "name": "nightly",
            "steps": [
                { "skill_name": "ingest", "scope_id": "scope-a" },
                { "skill_name": "report", "scope_id": "scope-b" }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(created["name"], "nightly");
    assert_eq!(created["owner"], ELENA);
    let id = created["loop_id"].as_str().unwrap();
    let steps = created["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["position"], 0);
    assert_eq!(steps[0]["skill_name"], "ingest");
    assert_eq!(steps[1]["position"], 1);

    // Read it back, and see it in the owner's list.
    let (status, read) = get(&app, &format!("/me/loops/{id}"), &elena).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read["loop_id"], id);

    let (_s, list) = get(&app, "/me/loops", &elena).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_non_owner_cannot_read_a_loop() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;
    let created = post(&app, "/me/loops", Some(&elena), json!({ "name": "mine" }))
        .await
        .1;
    let id = created["loop_id"].as_str().unwrap();
    let (status, _) = get(&app, &format!("/me/loops/{id}"), &peter).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_loop_with_an_authorized_step_runs_to_completion() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;

    let scope_id = approved_scope(&app, &elena, &peter).await;
    // A skill backed by the L1 reporting guest — elena (L2) clears the run gate.
    publish_and_bind(
        &app,
        &elena,
        &peter,
        &scope_id,
        "daily-report",
        "reporting",
        "L1",
    )
    .await;

    let created = post(
        &app,
        "/me/loops",
        Some(&elena),
        json!({ "name": "report-loop", "steps": [{ "skill_name": "daily-report", "scope_id": scope_id }] }),
    )
    .await
    .1;
    let id = created["loop_id"].as_str().unwrap().to_string();

    // Run it; the response carries the run id and an initial state.
    let (status, run) = post(
        &app,
        &format!("/me/loops/{id}/run"),
        Some(&elena),
        json!({ "input": {} }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let run_id = run["run_id"].as_str().unwrap().to_string();
    assert_eq!(run["loop_id"], id);

    // Poll until terminal: the authorized step completes, so the run completes.
    let final_run = poll_run(&app, &elena, &run_id).await;
    assert_eq!(final_run["status"], "completed");
    let steps = final_run["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0]["status"], "completed");
    assert_eq!(steps[0]["skill_name"], "daily-report");
}

#[tokio::test]
async fn the_run_gate_denies_an_under_cleared_step_and_fails_the_run() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;

    let scope_id = approved_scope(&app, &elena, &peter).await;
    // elena may BIND an L4-requiring skill (define ≠ run) — but cannot RUN it at L2.
    publish_and_bind(
        &app,
        &elena,
        &peter,
        &scope_id,
        "exec-only",
        "reporting",
        "L4",
    )
    .await;

    let created = post(
        &app,
        "/me/loops",
        Some(&elena),
        json!({ "name": "blocked", "steps": [{ "skill_name": "exec-only", "scope_id": scope_id }] }),
    )
    .await
    .1;
    let id = created["loop_id"].as_str().unwrap().to_string();

    let run = post(
        &app,
        &format!("/me/loops/{id}/run"),
        Some(&elena),
        json!({ "input": {} }),
    )
    .await
    .1;
    let run_id = run["run_id"].as_str().unwrap().to_string();

    let final_run = poll_run(&app, &elena, &run_id).await;
    assert_eq!(
        final_run["status"], "failed",
        "an unauthorized step fails the run"
    );
    assert_eq!(
        final_run["steps"][0]["status"], "denied",
        "the run gate denied it"
    );
}

#[tokio::test]
async fn a_skill_under_declaring_its_guest_clearance_is_denied() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;

    let scope_id = approved_scope(&app, &elena, &peter).await;
    // `sneaky` declares L1 but is backed by the valuation guest (policy floor L3).
    // elena (L2) may bind it (define ≠ run) and clears the *skill* floor (L1), but the
    // loop path also enforces the backing *guest*'s policy floor — so it is denied
    // (the same floor `POST /guests/valuation` enforces), and the guest never runs.
    publish_and_bind(&app, &elena, &peter, &scope_id, "sneaky", "valuation", "L1").await;

    let created = post(
        &app,
        "/me/loops",
        Some(&elena),
        json!({ "name": "sneaky-loop", "steps": [{ "skill_name": "sneaky", "scope_id": scope_id }] }),
    )
    .await
    .1;
    let id = created["loop_id"].as_str().unwrap().to_string();
    let run = post(
        &app,
        &format!("/me/loops/{id}/run"),
        Some(&elena),
        json!({ "input": {} }),
    )
    .await
    .1;
    let run_id = run["run_id"].as_str().unwrap().to_string();

    let final_run = poll_run(&app, &elena, &run_id).await;
    assert_eq!(final_run["status"], "failed");
    assert_eq!(
        final_run["steps"][0]["status"], "denied",
        "the backing guest's clearance floor is enforced at the loop gate"
    );
}

#[tokio::test]
async fn a_non_owner_cannot_run_a_loop() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;
    let created = post(
        &app,
        "/me/loops",
        Some(&elena),
        json!({ "name": "mine", "steps": [{ "skill_name": "x", "scope_id": "s" }] }),
    )
    .await
    .1;
    let id = created["loop_id"].as_str().unwrap();
    let (status, _) = post(
        &app,
        &format!("/me/loops/{id}/run"),
        Some(&peter),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn running_an_empty_loop_is_a_400() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let created = post(&app, "/me/loops", Some(&elena), json!({ "name": "empty" }))
        .await
        .1;
    let id = created["loop_id"].as_str().unwrap();
    let (status, err) = post(
        &app,
        &format!("/me/loops/{id}/run"),
        Some(&elena),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["kind"], "invalid_request");
}

#[tokio::test]
async fn running_an_unknown_loop_is_404() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let (status, _) = post(
        &app,
        "/me/loops/00000000-0000-0000-0000-000000000000/run",
        Some(&elena),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reading_an_unknown_run_is_404() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let (status, _) = get(&app, "/me/loops/runs/does-not-exist", &elena).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_non_owner_cannot_read_someone_elses_run() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;

    let scope_id = approved_scope(&app, &elena, &peter).await;
    publish_and_bind(
        &app,
        &elena,
        &peter,
        &scope_id,
        "daily-report",
        "reporting",
        "L1",
    )
    .await;
    let created = post(
        &app,
        "/me/loops",
        Some(&elena),
        json!({ "name": "r", "steps": [{ "skill_name": "daily-report", "scope_id": scope_id }] }),
    )
    .await
    .1;
    let id = created["loop_id"].as_str().unwrap().to_string();
    let run = post(
        &app,
        &format!("/me/loops/{id}/run"),
        Some(&elena),
        json!({ "input": {} }),
    )
    .await
    .1;
    let run_id = run["run_id"].as_str().unwrap();

    // Peter did not start this run.
    let (status, _) = get(&app, &format!("/me/loops/runs/{run_id}"), &peter).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
