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

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Duration;
use http_body_util::BodyExt;
use kanbrick_api::{router, AppState, InvocationCaps, McpBridge, ProviderFactory};
use kanbrick_auth::{JwtAuthenticator, LoginService};
use kanbrick_providers::{
    ChatProvider, ChatRequest, ChatResponse, ProviderError, ProviderKind, Role, StopReason, Usage,
};
use kanbrick_store::{seed, Migrator, Store};
use serde_json::{json, Value};
use tower::ServiceExt;

const SECRET: &[u8] = b"loops-suite-secret";

const ELENA: &str = "elena.ruiz@kanbrick.com"; // L2 — loop owner / scope grantee
const PETER: &str = "peter.nash@kanbrick.com"; // L4 — grantor + skill publisher

/// Seed the firm graph + financials and provision the elena/peter logins.
fn seeded() -> (tempfile::TempDir, Store, JwtAuthenticator) {
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
    (dir, store, jwt)
}

fn app() -> (tempfile::TempDir, axum::Router) {
    let (dir, store, jwt) = seeded();
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

// ── P11.4 — provider steps (host-injected key; step picks the model only) ─────

/// Save a provider key for the caller via the P9.3 custody route. The step never
/// carries a key; the run engine resolves it from here by the caller's identity.
async fn save_key(app: &axum::Router, token: &str, provider: &str, secret: &str) -> StatusCode {
    post(
        app,
        "/me/provider-keys",
        Some(token),
        json!({ "provider": provider, "label": "loop-key", "secret": secret }),
    )
    .await
    .0
}

#[tokio::test]
async fn a_provider_step_runs_with_the_host_injected_key() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;

    let scope_id = approved_scope(&app, &elena, &peter).await;
    // The skill provides authorization + scope (1A); provider_ref overrides execution.
    publish_and_bind(
        &app,
        &elena,
        &peter,
        &scope_id,
        "summarize",
        "reporting",
        "L1",
    )
    .await;
    // Elena saves her own key in custody — the step below carries no credential.
    assert_eq!(
        save_key(&app, &elena, "anthropic", "sk-elena-secret").await,
        StatusCode::OK
    );

    let created = post(
        &app,
        "/me/loops",
        Some(&elena),
        json!({ "name": "summarize-loop", "steps": [
            { "skill_name": "summarize", "scope_id": scope_id,
              "provider_ref": { "provider": "anthropic", "model": "claude-opus-4-8" } } ]}),
    )
    .await
    .1;
    let id = created["loop_id"].as_str().unwrap().to_string();
    // The definition surfaces the provider selection (model only — no key anywhere).
    assert_eq!(created["steps"][0]["provider"], "anthropic");
    assert_eq!(created["steps"][0]["model"], "claude-opus-4-8");

    let run = post(
        &app,
        &format!("/me/loops/{id}/run"),
        Some(&elena),
        json!({ "input": "summarize the portfolio" }),
    )
    .await
    .1;
    let run_id = run["run_id"].as_str().unwrap().to_string();
    let final_run = poll_run(&app, &elena, &run_id).await;
    assert_eq!(final_run["status"], "completed");
    assert_eq!(final_run["steps"][0]["status"], "completed");
}

#[tokio::test]
async fn a_provider_step_without_a_saved_key_fails_the_run() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;
    let scope_id = approved_scope(&app, &elena, &peter).await;
    publish_and_bind(
        &app,
        &elena,
        &peter,
        &scope_id,
        "summarize",
        "reporting",
        "L1",
    )
    .await;
    // No key saved for the caller — the host has nothing to inject, so the step fails
    // (it cannot fall back to a step-supplied credential, because there is none).

    let created = post(
        &app,
        "/me/loops",
        Some(&elena),
        json!({ "name": "no-key", "steps": [
            { "skill_name": "summarize", "scope_id": scope_id,
              "provider_ref": { "provider": "openai", "model": "gpt-4o" } } ]}),
    )
    .await
    .1;
    let id = created["loop_id"].as_str().unwrap().to_string();
    let run = post(
        &app,
        &format!("/me/loops/{id}/run"),
        Some(&elena),
        json!({ "input": "x" }),
    )
    .await
    .1;
    let run_id = run["run_id"].as_str().unwrap().to_string();
    let final_run = poll_run(&app, &elena, &run_id).await;
    assert_eq!(final_run["status"], "failed");
    assert_eq!(final_run["steps"][0]["status"], "failed");
    assert!(final_run["steps"][0]["detail"]
        .as_str()
        .unwrap()
        .contains("no openai key"));
}

/// A [`ProviderFactory`] that records the `(kind, key)` it was built with.
#[derive(Clone)]
struct RecordingFactory {
    seen: Arc<Mutex<Vec<(ProviderKind, String)>>>,
}

impl ProviderFactory for RecordingFactory {
    fn build(&self, kind: ProviderKind, api_key: &str) -> Box<dyn ChatProvider> {
        self.seen.lock().unwrap().push((kind, api_key.to_string()));
        Box::new(EchoLike { kind })
    }
}

/// A minimal [`ChatProvider`] returning the last user message (no network).
struct EchoLike {
    kind: ProviderKind,
}

impl ChatProvider for EchoLike {
    fn kind(&self) -> ProviderKind {
        self.kind
    }
    fn complete(&self, request: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        let content = request
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, Role::User))
            .map(|m| m.content.clone())
            .unwrap_or_default();
        Ok(ChatResponse {
            model: request.model.clone(),
            content,
            usage: Usage::default(),
            stop_reason: StopReason::EndTurn,
        })
    }
}

#[tokio::test]
async fn the_host_injects_the_callers_saved_key_into_the_provider() {
    let (_d, store, jwt) = seeded();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let factory: Arc<dyn ProviderFactory> = Arc::new(RecordingFactory { seen: seen.clone() });
    let app = router(
        AppState::new(store, jwt)
            .unwrap()
            .with_provider_factory(factory),
    );

    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;
    let scope_id = approved_scope(&app, &elena, &peter).await;
    publish_and_bind(
        &app,
        &elena,
        &peter,
        &scope_id,
        "summarize",
        "reporting",
        "L1",
    )
    .await;
    save_key(&app, &elena, "cerebras", "sk-cerebras-elena").await;

    let created = post(
        &app,
        "/me/loops",
        Some(&elena),
        json!({ "name": "rec", "steps": [
            { "skill_name": "summarize", "scope_id": scope_id,
              "provider_ref": { "provider": "cerebras", "model": "llama-3.3-70b" } } ]}),
    )
    .await
    .1;
    let id = created["loop_id"].as_str().unwrap().to_string();
    let run = post(
        &app,
        &format!("/me/loops/{id}/run"),
        Some(&elena),
        json!({ "input": "hi" }),
    )
    .await
    .1;
    let run_id = run["run_id"].as_str().unwrap().to_string();
    poll_run(&app, &elena, &run_id).await;

    // The factory received the caller's OWN saved key — host-resolved by identity,
    // never carried by the step (the step body had only provider + model).
    let recorded = seen.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1, "one provider built");
    assert_eq!(recorded[0].0, ProviderKind::Cerebras);
    assert_eq!(
        recorded[0].1, "sk-cerebras-elena",
        "the host injected the saved key"
    );
}

#[tokio::test]
async fn creating_a_provider_step_with_an_unknown_provider_is_400() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let (status, err) = post(
        &app,
        "/me/loops",
        Some(&elena),
        json!({ "name": "bad", "steps": [
            { "skill_name": "s", "scope_id": "sc",
              "provider_ref": { "provider": "gemini", "model": "x" } } ]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["kind"], "invalid_request");
}

#[tokio::test]
async fn creating_a_provider_step_without_a_model_is_400() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let (status, _) = post(
        &app,
        "/me/loops",
        Some(&elena),
        json!({ "name": "bad", "steps": [
            { "skill_name": "s", "scope_id": "sc",
              "provider_ref": { "provider": "openai", "model": "  " } } ]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ── P11.5 — MCP tool-call steps (host-minted cap; step names only the tool) ───

/// One recorded `call_tool`: what the bridge received, plus the identity the opaque
/// cap resolved to **host-side**. If `resolved_email` is the caller, the step named
/// only the tool — identity rode the cap, never the step body.
#[derive(Clone)]
struct ToolCall {
    resolved_email: Option<String>,
    tool: String,
    args: Value,
}

/// An [`McpBridge`] that records every call and resolves the opaque cap against the
/// **same** `InvocationCaps` the host minted it from (so the test can prove the cap
/// maps to the caller). A real bridge would relay the cap to the sidecar instead.
#[derive(Clone)]
struct RecordingBridge {
    caps: Arc<InvocationCaps>,
    seen: Arc<Mutex<Vec<ToolCall>>>,
}

impl McpBridge for RecordingBridge {
    fn call_tool(&self, cap: &str, tool: &str, args: &Value) -> Result<Value, String> {
        // Resolve while the cap is live (the engine revokes it the instant we return).
        let resolved_email = self.caps.resolve(cap).map(|c| c.email);
        self.seen.lock().unwrap().push(ToolCall {
            resolved_email,
            tool: tool.to_string(),
            args: args.clone(),
        });
        Ok(json!({ "ok": true, "tool": tool }))
    }
}

/// An [`McpBridge`] that always errors — stands in for an unknown/failed tool.
struct FailingBridge;

impl McpBridge for FailingBridge {
    fn call_tool(&self, _cap: &str, tool: &str, _args: &Value) -> Result<Value, String> {
        Err(format!("unknown tool {tool}"))
    }
}

#[tokio::test]
async fn a_tool_step_runs_to_completion_with_the_default_stub() {
    let (_d, app) = app(); // default StubMcpBridge — no network, canned echo.
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;
    let scope_id = approved_scope(&app, &elena, &peter).await;
    // The skill provides authorization + scope; tool_ref overrides execution.
    publish_and_bind(&app, &elena, &peter, &scope_id, "fetch", "reporting", "L1").await;

    let created = post(
        &app,
        "/me/loops",
        Some(&elena),
        json!({ "name": "tool-loop", "steps": [
            { "skill_name": "fetch", "scope_id": scope_id,
              "tool_ref": { "tool": "web.search", "args": { "q": "kanbrick" } } } ]}),
    )
    .await
    .1;
    // The definition surfaces the tool selection + parsed args (no credential anywhere).
    assert_eq!(created["steps"][0]["tool"], "web.search");
    assert_eq!(created["steps"][0]["tool_args"]["q"], "kanbrick");
    assert!(created["steps"][0]["provider"].is_null());
    let id = created["loop_id"].as_str().unwrap().to_string();

    let run = post(
        &app,
        &format!("/me/loops/{id}/run"),
        Some(&elena),
        json!({ "input": "go" }),
    )
    .await
    .1;
    let run_id = run["run_id"].as_str().unwrap().to_string();
    let final_run = poll_run(&app, &elena, &run_id).await;
    assert_eq!(final_run["status"], "completed");
    assert_eq!(final_run["steps"][0]["status"], "completed");
}

#[tokio::test]
async fn the_host_mints_a_caller_bound_cap_for_a_tool_step() {
    let (_d, store, jwt) = seeded();
    let state = AppState::new(store, jwt).unwrap();
    // Share the host's capability registry with the recording bridge so it can prove
    // the opaque cap resolves to the caller (a real sidecar cannot — it has no caps).
    let caps = state.caps.clone();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let bridge: Arc<dyn McpBridge> = Arc::new(RecordingBridge {
        caps: caps.clone(),
        seen: seen.clone(),
    });
    let app = router(state.with_mcp_bridge(bridge));

    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;
    let scope_id = approved_scope(&app, &elena, &peter).await;
    publish_and_bind(&app, &elena, &peter, &scope_id, "fetch", "reporting", "L1").await;

    let created = post(
        &app,
        "/me/loops",
        Some(&elena),
        json!({ "name": "rec-tool", "steps": [
            { "skill_name": "fetch", "scope_id": scope_id,
              "tool_ref": { "tool": "web.search", "args": { "q": "kanbrick" } } } ]}),
    )
    .await
    .1;
    let id = created["loop_id"].as_str().unwrap().to_string();
    let run = post(
        &app,
        &format!("/me/loops/{id}/run"),
        Some(&elena),
        json!({ "input": "go" }),
    )
    .await
    .1;
    let run_id = run["run_id"].as_str().unwrap().to_string();
    let final_run = poll_run(&app, &elena, &run_id).await;
    assert_eq!(final_run["status"], "completed");

    // The bridge saw the OPAQUE cap (resolving host-side to elena), the tool, and the
    // args — static args plus the piped payload under "input". Identity is never in
    // the step body; it rides the cap, which only the host can resolve.
    let recorded = seen.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1, "one tool call");
    assert_eq!(
        recorded[0].resolved_email.as_deref(),
        Some(ELENA),
        "the host minted a cap bound to the caller; the bridge resolves it host-side"
    );
    assert_eq!(recorded[0].tool, "web.search");
    assert_eq!(
        recorded[0].args["q"], "kanbrick",
        "static tool args preserved"
    );
    assert_eq!(
        recorded[0].args["input"], "go",
        "the piped payload rides under input"
    );
}

#[tokio::test]
async fn a_tool_step_whose_tool_errors_fails_the_run() {
    let (_d, store, jwt) = seeded();
    let app = router(
        AppState::new(store, jwt)
            .unwrap()
            .with_mcp_bridge(Arc::new(FailingBridge)),
    );
    let elena = login(&app, ELENA, "pw2").await;
    let peter = login(&app, PETER, "pw4").await;
    let scope_id = approved_scope(&app, &elena, &peter).await;
    publish_and_bind(&app, &elena, &peter, &scope_id, "fetch", "reporting", "L1").await;

    let created = post(
        &app,
        "/me/loops",
        Some(&elena),
        json!({ "name": "bad-tool", "steps": [
            { "skill_name": "fetch", "scope_id": scope_id,
              "tool_ref": { "tool": "missing.tool" } } ]}),
    )
    .await
    .1;
    let id = created["loop_id"].as_str().unwrap().to_string();
    let run = post(
        &app,
        &format!("/me/loops/{id}/run"),
        Some(&elena),
        json!({ "input": "x" }),
    )
    .await
    .1;
    let run_id = run["run_id"].as_str().unwrap().to_string();
    let final_run = poll_run(&app, &elena, &run_id).await;
    assert_eq!(final_run["status"], "failed");
    assert_eq!(final_run["steps"][0]["status"], "failed");
    assert!(final_run["steps"][0]["detail"]
        .as_str()
        .unwrap()
        .contains("unknown tool"));
}

#[tokio::test]
async fn creating_a_tool_step_with_an_empty_tool_is_400() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let (status, err) = post(
        &app,
        "/me/loops",
        Some(&elena),
        json!({ "name": "bad", "steps": [
            { "skill_name": "s", "scope_id": "sc", "tool_ref": { "tool": "  " } } ]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["kind"], "invalid_request");
}

#[tokio::test]
async fn creating_a_tool_step_with_non_object_args_is_400() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let (status, _) = post(
        &app,
        "/me/loops",
        Some(&elena),
        json!({ "name": "bad", "steps": [
            { "skill_name": "s", "scope_id": "sc",
              "tool_ref": { "tool": "t", "args": [1, 2, 3] } } ]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn creating_a_step_with_both_provider_and_tool_is_400() {
    let (_d, app) = app();
    let elena = login(&app, ELENA, "pw2").await;
    let (status, err) = post(
        &app,
        "/me/loops",
        Some(&elena),
        json!({ "name": "bad", "steps": [
            { "skill_name": "s", "scope_id": "sc",
              "provider_ref": { "provider": "openai", "model": "gpt-4o" },
              "tool_ref": { "tool": "web.search" } } ]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["kind"], "invalid_request");
}
