//! Loop run-and-watch IPC (P11.7).
//!
//! The user-facing half of the loop ecosystem: list the caller's loops
//! (`GET /me/loops`), run one (`POST /me/loops/{id}/run`), and stream its per-step
//! status live (`GET /me/loops/runs/{id}`) over a [`tauri::ipc::Channel`] until the
//! run reaches a terminal state. It reuses the visualizer/messenger poller verbatim
//! — std thread + `block_on` + `channel.send` + an `AtomicBool` cancel — but the
//! run watch **self-stops** once the run is no longer `running`, instead of polling
//! forever.
//!
//! Identity stays host-authoritative (ADR-0016): every call attaches the Bearer from
//! the host-held [`Session`](crate::auth::Session) via the auth bridge; the webview
//! supplies only the loop/run id and the optional input. The DTOs mirror
//! `kanbrick-api`'s `LoopDto`/`RunDto` 1:1 (and the TS types in `src/api.ts`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

use crate::auth::{authed_get, authed_post, Session};

/// How often the run-watch loop polls the run status.
const POLL_INTERVAL: Duration = Duration::from_millis(700);
/// Sub-interval tick so a `stop_run_watch` is observed promptly between polls.
const POLL_TICK: Duration = Duration::from_millis(120);

/// Per-watch cancel flags keyed by the id returned to the webview. Aliased to keep
/// the `Mutex` field under clippy's `type_complexity` bar (as the other hubs do).
type WatchRegistry = HashMap<Uuid, Arc<AtomicBool>>;

/// One step of a loop definition, mirroring `kanbrick-api`'s `LoopStepDto`. The kind
/// fields (`provider`/`model` for a provider step, `tool`/`tool_args` for an MCP tool
/// step) are present only for their step kind; all empty → a guest step. The webview
/// only reflects what the host sends (ADR-0016); it derives no identity from them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopStepView {
    pub position: i64,
    pub skill_name: String,
    pub scope_id: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub tool_args: Option<JsonValue>,
}

/// A loop definition, mirroring `kanbrick-api`'s `LoopDto`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopSummary {
    pub loop_id: String,
    pub name: String,
    pub owner: String,
    pub created_at: String,
    pub steps: Vec<LoopStepView>,
}

/// One step's live run status, mirroring `kanbrick-api`'s `RunStepDto`. `status` is
/// `"pending"|"running"|"completed"|"denied"|"failed"|"timed_out"`; `detail` carries
/// the reason for a denied/failed step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStepView {
    pub position: i64,
    pub skill_name: String,
    pub scope_id: String,
    pub status: String,
    #[serde(default)]
    pub detail: Option<String>,
    /// Step kind (P11.4/P11.5), so the run view can badge each step; present only for
    /// the matching kind (provider step → provider/model; MCP tool step → tool).
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
}

/// A loop run's live state, mirroring `kanbrick-api`'s `RunDto`. `status` is
/// `"running"|"completed"|"failed"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunView {
    pub run_id: String,
    pub loop_id: String,
    pub started_at: String,
    pub status: String,
    pub steps: Vec<RunStepView>,
}

/// Events streamed to the webview over the Channel (internally tagged on `event`,
/// mirrored by the `RunEvent` union in `src/api.ts`).
#[derive(Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RunEvent {
    /// A fresh snapshot of the run's per-step status.
    Snapshot {
        /// The current run state.
        run: RunView,
    },
    /// A transient fetch error; the loop keeps polling so it self-heals (e.g. the
    /// run is not yet visible right after submission).
    Error {
        /// Human-readable reason.
        message: String,
    },
    /// The watch ended — either the run reached a terminal state or it was cancelled.
    Stopped,
}

/// Host-side registry of live run watches so each can be cancelled. Mirrors the
/// visualizer/messenger watch registries.
#[derive(Default)]
pub struct LoopRunnerHub {
    watches: Arc<Mutex<WatchRegistry>>,
}

impl LoopRunnerHub {
    fn register(&self, id: Uuid, cancel: Arc<AtomicBool>) {
        self.watches
            .lock()
            .expect("loop-run watch lock")
            .insert(id, cancel);
    }

    fn cancel(&self, id: Uuid) {
        if let Some(flag) = self.watches.lock().expect("loop-run watch lock").get(&id) {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

/// Body for `POST /me/loops/{id}/run`.
#[derive(Serialize)]
struct RunBody {
    input: JsonValue,
}

/// A provider selection for a provider step (P11.4), mirroring `kanbrick-api`'s
/// `ProviderRefInput`. Carries the model only — never a credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRef {
    pub provider: String,
    pub model: String,
}

/// An MCP tool selection for a tool step (P11.5), mirroring `kanbrick-api`'s
/// `ToolRefInput`. Names the tool + optional static args only — never an identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRef {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<JsonValue>,
}

/// One step to author, mirroring `kanbrick-api`'s `StepInput`. A step is at most one
/// kind: guest (neither ref), provider (`provider_ref`), or MCP tool (`tool_ref`); the
/// server rejects a step that sets more than one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepSpec {
    pub skill_name: String,
    pub scope_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_ref: Option<ProviderRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_ref: Option<ToolRef>,
}

/// Body for `POST /me/loops` — author an ordered loop.
#[derive(Serialize)]
struct CreateBody {
    name: String,
    steps: Vec<StepSpec>,
}

/// A 401 clears the host session so the UI falls back to login.
fn session_expired(app: &AppHandle) -> String {
    app.state::<Session>().clear();
    "session expired — please sign in again".to_string()
}

/// Fetch one run's live state from the sidecar through the auth bridge. Shared by the
/// run-watch loop; resolves identity host-side on every poll.
async fn fetch_run(app: &AppHandle, run_id: &str) -> Result<RunView, String> {
    let response = authed_get(app, &format!("/me/loops/runs/{run_id}")).await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(session_expired(app));
    }
    if !response.status().is_success() {
        return Err(format!("could not load run ({})", response.status()));
    }
    response
        .json::<RunView>()
        .await
        .map_err(|e| format!("unexpected run response: {e}"))
}

/// `invoke('list_loops')` — the caller's loops via `GET /me/loops` through the auth
/// bridge. Identity is derived entirely from the host-held token.
#[tauri::command]
pub async fn list_loops(app: AppHandle) -> Result<Vec<LoopSummary>, String> {
    let response = authed_get(&app, "/me/loops").await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(session_expired(&app));
    }
    if !response.status().is_success() {
        return Err(format!("could not load loops ({})", response.status()));
    }
    response
        .json::<Vec<LoopSummary>>()
        .await
        .map_err(|e| format!("unexpected loops response: {e}"))
}

/// `invoke('run_loop', { loopId, input })` — run a loop via `POST /me/loops/{id}/run`.
/// The webview supplies only the loop id + optional input; the host injects the
/// Bearer and the server gates each step at run time. Returns the initial run state
/// (carrying the `run_id` to watch). A missing input defaults to an empty object.
#[tauri::command]
pub async fn run_loop(
    app: AppHandle,
    loop_id: String,
    input: Option<JsonValue>,
) -> Result<RunView, String> {
    let body = RunBody {
        input: input.unwrap_or_else(|| JsonValue::Object(Default::default())),
    };
    let response = authed_post(&app, &format!("/me/loops/{loop_id}/run"), &body).await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(session_expired(&app));
    }
    if !response.status().is_success() {
        return Err(format!("could not run loop ({})", response.status()));
    }
    response
        .json::<RunView>()
        .await
        .map_err(|e| format!("unexpected run response: {e}"))
}

/// `invoke('create_loop', { name, steps })` — author a loop via `POST /me/loops`. Each
/// step names a bound skill + scope and is guest, provider, or MCP-tool; the server
/// validates (a step is at most one kind, an unknown provider/empty tool is a 400).
/// Returns the created loop, which then appears in the run-and-watch picker above.
#[tauri::command]
pub async fn create_loop(
    app: AppHandle,
    name: String,
    steps: Vec<StepSpec>,
) -> Result<LoopSummary, String> {
    let response = authed_post(&app, "/me/loops", &CreateBody { name, steps }).await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(session_expired(&app));
    }
    if !response.status().is_success() {
        let status = response.status();
        let message = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
            .unwrap_or_else(|| format!("could not create loop ({status})"));
        return Err(message);
    }
    response
        .json::<LoopSummary>()
        .await
        .map_err(|e| format!("unexpected create-loop response: {e}"))
}

/// `invoke('watch_run', { runId, channel })` — stream a run's per-step status to
/// `channel` until it reaches a terminal state (or [`stop_run_watch`]). Returns a
/// watch id. The poll loop resolves identity host-side on every tick; the webview
/// passes only the run id + channel. Requires a signed-in session.
#[tauri::command]
pub fn watch_run(
    app: AppHandle,
    session: tauri::State<'_, Session>,
    hub: tauri::State<'_, LoopRunnerHub>,
    run_id: String,
    channel: tauri::ipc::Channel<RunEvent>,
) -> Result<String, String> {
    if session.token().is_none() {
        return Err("not signed in".to_string());
    }

    let watch_id = Uuid::new_v4();
    let cancel = Arc::new(AtomicBool::new(false));
    hub.register(watch_id, cancel.clone());
    let watches = hub.watches.clone();

    // Poll off the UI thread, bridging to the async auth-bridge fetch with
    // `block_on` (the std-thread streaming pattern the visualizer/messenger use).
    std::thread::spawn(move || {
        while !cancel.load(Ordering::Relaxed) {
            match tauri::async_runtime::block_on(fetch_run(&app, &run_id)) {
                Ok(run) => {
                    // Capture terminality before the value is moved into the event.
                    let terminal = run.status != "running";
                    if channel.send(RunEvent::Snapshot { run }).is_err() {
                        break; // the webview dropped the channel
                    }
                    if terminal {
                        break; // self-stop: the run is complete/failed
                    }
                }
                Err(message) => {
                    // Keep polling on a transient error so the watch self-heals.
                    if channel.send(RunEvent::Error { message }).is_err() {
                        break;
                    }
                }
            }
            // Wait one interval in short ticks so a stop is observed promptly.
            let mut waited = Duration::ZERO;
            while waited < POLL_INTERVAL && !cancel.load(Ordering::Relaxed) {
                std::thread::sleep(POLL_TICK);
                waited += POLL_TICK;
            }
        }
        let _ = channel.send(RunEvent::Stopped);
        watches
            .lock()
            .expect("loop-run watch lock")
            .remove(&watch_id);
    });

    Ok(watch_id.to_string())
}

/// `invoke('stop_run_watch', { watch })` — signal the run-watch loop to stop.
#[tauri::command]
pub fn stop_run_watch(hub: tauri::State<'_, LoopRunnerHub>, watch: String) {
    if let Ok(id) = Uuid::parse_str(&watch) {
        hub.cancel(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_summary_mirrors_the_api_json() {
        // The exact shape kanbrick-api's `GET /me/loops` emits for one loop.
        let json = serde_json::json!({
            "loop_id": "L1",
            "name": "nightly",
            "owner": "elena.ruiz@kanbrick.com",
            "created_at": "2026-06-28T00:00:00+00:00",
            "steps": [
                { "position": 0, "skill_name": "daily-report", "scope_id": "S1" },
                { "position": 1, "skill_name": "summarize", "scope_id": "S1",
                  "provider": "anthropic", "model": "claude-opus-4-8" },
                { "position": 2, "skill_name": "fetch", "scope_id": "S1",
                  "tool": "web.search", "tool_args": { "q": "kanbrick" } }
            ]
        });
        let l: LoopSummary = serde_json::from_value(json).unwrap();
        assert_eq!(l.name, "nightly");
        assert_eq!(l.owner, "elena.ruiz@kanbrick.com");
        assert_eq!(l.steps.len(), 3);
        // Guest step: every kind field absent → None.
        assert_eq!(l.steps[0].skill_name, "daily-report");
        assert_eq!(l.steps[0].provider, None);
        assert_eq!(l.steps[0].tool, None);
        // Provider step: provider/model present, tool absent.
        assert_eq!(l.steps[1].provider.as_deref(), Some("anthropic"));
        assert_eq!(l.steps[1].model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(l.steps[1].tool, None);
        // MCP tool step: tool + tool_args present.
        assert_eq!(l.steps[2].tool.as_deref(), Some("web.search"));
        assert_eq!(l.steps[2].tool_args.as_ref().unwrap()["q"], "kanbrick");
    }

    #[test]
    fn run_view_mirrors_the_api_json_including_optional_detail() {
        // A denied step carries a `detail`; a completed step omits it.
        let json = serde_json::json!({
            "run_id": "R1",
            "loop_id": "L1",
            "started_at": "2026-06-28T00:00:01+00:00",
            "status": "failed",
            "steps": [
                { "position": 0, "skill_name": "a", "scope_id": "S1", "status": "completed",
                  "provider": "anthropic", "model": "claude-opus-4-8" },
                { "position": 1, "skill_name": "b", "scope_id": "S1", "status": "denied",
                  "detail": "caller clearance below the valuation guest floor" }
            ]
        });
        let r: RunView = serde_json::from_value(json).unwrap();
        assert_eq!(r.status, "failed");
        assert_eq!(r.steps.len(), 2);
        assert_eq!(r.steps[0].status, "completed");
        assert_eq!(r.steps[0].detail, None);
        // The kind fields ride through the run step (provider step here).
        assert_eq!(r.steps[0].provider.as_deref(), Some("anthropic"));
        assert_eq!(r.steps[0].model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(r.steps[1].status, "denied");
        assert_eq!(r.steps[1].provider, None); // guest step
        assert!(r.steps[1].detail.as_deref().unwrap().contains("floor"));
    }

    #[test]
    fn run_event_serializes_with_its_tag() {
        let run = RunView {
            run_id: "R1".to_string(),
            loop_id: "L1".to_string(),
            started_at: "t".to_string(),
            status: "running".to_string(),
            steps: vec![],
        };
        let value = serde_json::to_value(RunEvent::Snapshot { run }).unwrap();
        assert_eq!(value["event"], "snapshot");
        assert_eq!(value["run"]["status"], "running");
        assert_eq!(
            serde_json::to_value(RunEvent::Stopped).unwrap()["event"],
            "stopped"
        );
    }

    #[test]
    fn cancel_sets_the_registered_flag() {
        let hub = LoopRunnerHub::default();
        let id = Uuid::new_v4();
        let flag = Arc::new(AtomicBool::new(false));
        hub.register(id, flag.clone());
        hub.cancel(id);
        assert!(flag.load(Ordering::Relaxed));
    }
}
