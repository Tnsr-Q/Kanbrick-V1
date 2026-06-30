//! `/me/loops` — the loop run engine (P11.3, ADR-0013).
//!
//! A **loop** is an owned, ordered pipeline of steps; each step names a *skill* and
//! the `scope_id` it runs under. This module is the *thin compiler* the epic calls
//! for: it builds nothing the mesh already has, it **compiles** a persisted loop
//! ([`kanbrick_store::loop_registry`]) onto the existing
//! [`Scheduler`](kanbrick_mesh::Scheduler) and gates **each step at run time**
//! through [`ScopeGrants::authorize_skill`].
//!
//! ## Run flow
//!
//! `POST /me/loops/{id}/run` resolves the caller's host base scope once
//! ([`ClearanceScope::resolve`]), spawns a background executor, and returns a
//! `run_id`. The executor walks the steps in order; for each it:
//!
//! 1. calls `authorize_skill(caller, base, scope_id, skill_name, now)` — the run
//!    gate (active+unexpired scope, caller is the grantee, clearance ≥ the skill's
//!    floor). A rejection marks the step **denied** and stops the run.
//! 2. on authorization, runs the step — one of **three kinds** (the polymorphic loop
//!    step; resolved in priority order tool > provider > guest). No kind carries a
//!    credential or an identity (the cap / host-resolved key stays host-side), and
//!    each step's output payload pipes into the next:
//!    * a **guest step** schedules the skill's `guest` on the `Scheduler` under the
//!      host-authoritative caller context and waits for its terminal `TaskStatus`;
//!    * a **provider step** (P11.4, ADR-0019) runs an LLM completion instead:
//!      `provider_ref` selects the model only, and the host resolves the caller's key
//!      from custody **by the caller's identity** and injects it into a
//!      [`ProviderFactory`]-built provider;
//!    * an **MCP tool-call step** (P11.5, ADR-0020) runs an external tool via the
//!      injected [`McpBridge`]: the host mints a per-invocation capability bound to
//!      the caller's `FirmContext` ([`InvocationCaps::mint`]), hands the bridge **only**
//!      the opaque cap + the tool + the args the scope authorizes, and **revokes the
//!      cap** the moment the call returns.
//!
//! `GET /me/loops/runs/{id}` reports the per-step status live. Run history is kept
//! **in-process** for now (the [`LoopRunRegistry`]); persisting it so it survives a
//! restart is a deferred companion of P11.5. The provider step's live egress (real
//! adapter + allowlist/DLP gate) lives behind the injected factory (ADR-0019); the
//! MCP tool step's live sidecar lives behind the injected bridge (ADR-0020) — this
//! slice ships the no-network echo default for both.
//!
//! Identity stays host-authoritative (ADR-0002/0016): the loop `owner`, the run
//! `caller`, and every guest invocation use the validated [`AuthedContext`]
//! `FirmContext`, never a body field. `authorize_skill` audits each authorized step
//! under that identity; the create/run mutations are audited here.
//!
//! **Scope note:** `authorize_skill` returns `(Skill, ProjectScope)`. The run *gate*
//! is the authorization itself; the composed `ProjectScope` (the additive grant
//! visibility) is not yet threaded into the guest's own graph queries — those run
//! under the caller's base clearance today. Applying the composed scope inside a
//! guest invocation needs a mesh seam and is tracked for a later slice (ADR-0013).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use kanbrick_auth::{require_clearance, AuditLog, ClearanceScope};
use kanbrick_core::abi::GuestRequest;
use kanbrick_core::{ClearanceLevel, FirmContext};
use kanbrick_discovery::ScopeGrants;
use kanbrick_mesh::{RetryPolicy, Scheduler, TaskStatus};
use kanbrick_providers::{ChatRequest, ProviderKeyStore, ProviderKind};
use kanbrick_store::{
    create_loop, get_loop, list_loops_for_owner, loop_steps, read_guest_policy, LoopRecord,
    LoopStepRecord, LoopStepSpec, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::provider_runtime::{self, ProviderFactory};
use crate::tool_runtime::McpBridge;
use crate::{ApiError, AppState, AuthedContext, InvocationCaps};

/// Minimum clearance to manage one's own loops. The floor (L1) — any authenticated
/// employee may author/run loops; the real bar is per-step `authorize_skill` (the
/// run gate) plus loop ownership, not this clearance step.
const MANAGE_LOOPS_CLEARANCE: ClearanceLevel = ClearanceLevel::L1;

/// Wall-clock budget for one step's guest execution. The mesh kills an overrun and
/// reports `TimedOut`, which fails the run at that step.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the executor waits for a step to reach a terminal status. A hair beyond
/// [`STEP_TIMEOUT`] so a timed-out task is observed as `TimedOut`, not a wait giveup.
const STEP_WAIT_BUDGET: Duration = Duration::from_secs(35);

/// Wall-clock budget for an entire loop run, enforced at step boundaries: before each
/// step the executor checks this deadline, so a long pipeline can't accrue
/// `steps × STEP_WAIT_BUDGET` unbounded. Comfortably above a single step's budget so a
/// normal multi-step loop never trips it; a run that does is failed at the next step.
const TOTAL_RUN_TIMEOUT: Duration = Duration::from_secs(300);

/// Cap on the in-process run registry. The oldest run is evicted once the registry
/// grows past this, so a long-lived node's run history can't grow without bound
/// (durable history is P11.5). Far above any realistic in-flight count, so the evicted
/// run is virtually always long-terminal.
const MAX_RETAINED_RUNS: usize = 512;

/// TTL of the per-invocation capability minted for an MCP tool-call step (P11.5). It
/// need only outlast the single bridge call; the cap is revoked the moment the call
/// returns, mirroring the executor cap in `invoke_guest` (`lib.rs`).
const TOOL_CAP_TTL: Duration = Duration::from_secs(60);

// ── In-process run registry (durable run history is P11.5) ───────────────────

/// Live, in-process state of every recent loop run on this node. Cheaply cloneable
/// (the state is behind an `Arc<Mutex<…>>`), so it rides in [`AppState`]. Bounded to
/// [`MAX_RETAINED_RUNS`] — the oldest run is evicted past the cap so history can't grow
/// without bound (durable history is P11.5).
#[derive(Clone, Default)]
pub struct LoopRunRegistry {
    inner: Arc<Mutex<Inner>>,
}

/// The registry's guarded state: the runs by id, plus their insertion order (oldest
/// first) so eviction past the cap is FIFO and O(1).
#[derive(Default)]
struct Inner {
    runs: HashMap<String, RunState>,
    order: VecDeque<String>,
}

#[derive(Clone)]
struct RunState {
    run_id: String,
    loop_id: String,
    caller: String,
    started_at: String,
    status: RunStatus,
    steps: Vec<StepState>,
}

#[derive(Clone, Copy, PartialEq)]
enum RunStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Clone)]
struct StepState {
    position: i64,
    skill_name: String,
    scope_id: String,
    status: StepOutcome,
    /// Step kind, captured from the `(:LoopStep)` so the run view can badge each step
    /// (guest/provider/mcp-tool) — empty strings for the dimensions a step doesn't use
    /// (mirrors `LoopStepRecord`).
    provider: String,
    model: String,
    tool: String,
}

/// Per-step outcome — mirrors the mesh [`TaskStatus`], plus `Denied` (the run gate
/// rejected the step before it ran) and `Pending` (not yet reached).
#[derive(Clone)]
enum StepOutcome {
    Pending,
    Running,
    Completed,
    Failed(String),
    Denied(String),
    TimedOut,
}

impl LoopRunRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a new run, evicting the oldest once the registry is over capacity
    /// ([`MAX_RETAINED_RUNS`]).
    fn insert(&self, run: RunState) {
        let mut inner = self.inner.lock().expect("loop-run lock");
        let run_id = run.run_id.clone();
        if inner.runs.insert(run_id.clone(), run).is_none() {
            inner.order.push_back(run_id);
        }
        while inner.order.len() > MAX_RETAINED_RUNS {
            if let Some(oldest) = inner.order.pop_front() {
                inner.runs.remove(&oldest);
            }
        }
    }

    fn snapshot(&self, run_id: &str) -> Option<RunState> {
        self.inner
            .lock()
            .expect("loop-run lock")
            .runs
            .get(run_id)
            .cloned()
    }

    fn set_step(&self, run_id: &str, position: i64, outcome: StepOutcome) {
        let mut inner = self.inner.lock().expect("loop-run lock");
        if let Some(run) = inner.runs.get_mut(run_id) {
            if let Some(step) = run.steps.iter_mut().find(|s| s.position == position) {
                step.status = outcome;
            }
        }
    }

    fn set_run_status(&self, run_id: &str, status: RunStatus) {
        let mut inner = self.inner.lock().expect("loop-run lock");
        if let Some(run) = inner.runs.get_mut(run_id) {
            run.status = status;
        }
    }
}

impl RunState {
    fn new(run_id: &str, loop_id: &str, caller: &str, steps: &[LoopStepRecord]) -> Self {
        RunState {
            run_id: run_id.to_string(),
            loop_id: loop_id.to_string(),
            caller: caller.to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            status: RunStatus::Running,
            steps: steps
                .iter()
                .map(|s| StepState {
                    position: s.position,
                    skill_name: s.skill_name.clone(),
                    scope_id: s.scope_id.clone(),
                    status: StepOutcome::Pending,
                    provider: s.provider.clone(),
                    model: s.model.clone(),
                    tool: s.tool.clone(),
                })
                .collect(),
        }
    }
}

// ── The compiler / executor ──────────────────────────────────────────────────

/// Execute a loop's steps sequentially, gating each through `authorize_skill`. A
/// **guest step** runs the bound skill's WASM guest on the `Scheduler`; a **provider
/// step** (P11.4) runs an LLM completion via the host-injected [`ProviderFactory`],
/// with the caller's key resolved from custody by the caller's identity; an **MCP
/// tool-call step** (P11.5) calls an external tool via the host-injected [`McpBridge`]
/// under a per-invocation capability minted from `caps` and bound to the caller. Runs
/// on a background thread; it communicates progress only through the shared
/// [`LoopRunRegistry`].
// The executor inherently carries the full run spec — the engine handles (store,
// scheduler, registry, custody, provider factory, capability registry, MCP bridge)
// plus the run's identity, base scope, steps, and input; a context struct would only
// relocate the arity.
#[allow(clippy::too_many_arguments)]
fn execute_loop(
    store: Arc<Store>,
    scheduler: Arc<Scheduler>,
    registry: LoopRunRegistry,
    provider_keys: Arc<dyn ProviderKeyStore>,
    provider_factory: Arc<dyn ProviderFactory>,
    caps: Arc<InvocationCaps>,
    mcp_bridge: Arc<dyn McpBridge>,
    run_id: String,
    caller: FirmContext,
    base: ClearanceScope,
    steps: Vec<LoopStepRecord>,
    initial_input: JsonValue,
) {
    let grants = ScopeGrants::new(&store);
    let mut payload = initial_input;
    let mut failed = false;
    // Whole-run budget: a runaway loop must not accrue `steps × STEP_WAIT_BUDGET`. The
    // per-step waits already bound each step; this caps the run as a whole, checked at
    // each step boundary (a step in flight is itself bounded by STEP_WAIT_BUDGET).
    let deadline = Instant::now() + TOTAL_RUN_TIMEOUT;

    for step in &steps {
        if Instant::now() >= deadline {
            registry.set_step(
                &run_id,
                step.position,
                StepOutcome::Failed("run exceeded the total-run timeout".to_string()),
            );
            failed = true;
            break;
        }
        registry.set_step(&run_id, step.position, StepOutcome::Running);
        // The run gate (deferred from P11.2b): a fresh `base` per step, since
        // `authorize_skill` consumes it. ACTIVE+unexpired scope, caller is the
        // grantee, clearance ≥ the skill's floor — else the step is denied.
        let skill = match grants.authorize_skill(
            &caller,
            base.clone(),
            &step.scope_id,
            &step.skill_name,
            chrono::Utc::now(),
        ) {
            Ok((skill, _composed)) => skill,
            Err(e) => {
                registry.set_step(&run_id, step.position, StepOutcome::Denied(e.to_string()));
                failed = true;
                break;
            }
        };

        if !step.tool.is_empty() {
            // ── MCP tool-call step (P11.5, ADR-0020): run an external tool via the
            // managed-sidecar bridge instead of a guest or an LLM. The same
            // `authorize_skill` gate (above) covers it — the skill supplies the scope +
            // clearance floor. The step names ONLY the tool + args, never an identity:
            // the host mints a per-invocation capability bound to the caller's
            // `FirmContext`, hands the bridge the *opaque* cap, and revokes it the
            // moment the call returns (mirroring `invoke_guest`'s executor cap). The
            // tool + args come from the step + the piped payload; identity rides the
            // cap, host-side — the bridge/sidecar never sees the identity bytes.
            let cap = caps.mint(caller.clone(), TOOL_CAP_TTL);
            let args = build_tool_args(&step.tool_args, &payload);
            let result = mcp_bridge.call_tool(&cap, &step.tool, &args);
            // Revoke regardless of outcome so a leaked cap cannot be replayed after.
            caps.revoke(&cap);
            match result {
                Ok(value) => {
                    // The host (not the sidecar) applies the result: pipe it onward.
                    payload = value;
                    registry.set_step(&run_id, step.position, StepOutcome::Completed);
                }
                Err(e) => {
                    registry.set_step(&run_id, step.position, StepOutcome::Failed(e));
                    failed = true;
                    break;
                }
            }
        } else if step.provider.is_empty() {
            // ── Guest step: run the bound skill's WASM guest on the Scheduler. ──
            //
            // Defense-in-depth: the loop path must enforce the same guest clearance
            // floor as `POST /guests/{name}`. `authorize_skill` checked the *skill*'s
            // declared clearance; also require the caller to meet the backing
            // *guest*'s policy floor, so a skill that under-declares its guest's
            // clearance can't reach a higher-floor guest through a loop. (Each guest
            // self-enforces too, but this keeps the floor uniform and fails closed.)
            match read_guest_policy(&store, &skill.guest) {
                Ok(Some(policy)) if caller.clearance < policy.min_clearance => {
                    registry.set_step(
                        &run_id,
                        step.position,
                        StepOutcome::Denied(format!(
                            "caller clearance below the {} guest floor",
                            skill.guest
                        )),
                    );
                    failed = true;
                    break;
                }
                // Policy satisfied, or unknown guest (scheduler reports GuestNotFound).
                Ok(_) => {}
                Err(e) => {
                    registry.set_step(&run_id, step.position, StepOutcome::Failed(e.to_string()));
                    failed = true;
                    break;
                }
            }

            // Compile the step to a scheduled guest invocation and pipe output→input.
            let request = GuestRequest::new(payload.clone());
            let task = scheduler.schedule_with_retry(
                &skill.guest,
                &caller,
                &request,
                Some(STEP_TIMEOUT),
                RetryPolicy::none(),
            );
            match scheduler.wait(task, STEP_WAIT_BUDGET) {
                Some(TaskStatus::Completed(response)) => {
                    payload = response.payload;
                    registry.set_step(&run_id, step.position, StepOutcome::Completed);
                }
                Some(TaskStatus::TimedOut) => {
                    registry.set_step(&run_id, step.position, StepOutcome::TimedOut);
                    failed = true;
                    break;
                }
                Some(TaskStatus::Failed(msg)) => {
                    registry.set_step(&run_id, step.position, StepOutcome::Failed(msg));
                    failed = true;
                    break;
                }
                // Non-terminal or unknown (wait budget elapsed): treat as a failure.
                _ => {
                    registry.set_step(
                        &run_id,
                        step.position,
                        StepOutcome::Failed("step did not reach a terminal status".to_string()),
                    );
                    failed = true;
                    break;
                }
            }
        } else {
            // ── Provider step (P11.4, ADR-0019): run an LLM completion instead of a
            // guest. The step's provider/model selects the model ONLY; the host
            // resolves the caller's key from custody by the caller's identity and
            // injects it into the provider — a step can never supply a credential or
            // an identity (ADR-0002). Egress (the real adapter + allowlist/DLP gate)
            // lives behind the injected `provider_factory`; the default echoes.
            let Some(kind) = provider_runtime::parse_provider(&step.provider) else {
                registry.set_step(
                    &run_id,
                    step.position,
                    StepOutcome::Failed(format!("unknown provider {}", step.provider)),
                );
                failed = true;
                break;
            };
            let key = match resolve_provider_key(&provider_keys, caller.user_id, kind) {
                Ok(Some(secret)) => secret,
                Ok(None) => {
                    registry.set_step(
                        &run_id,
                        step.position,
                        StepOutcome::Failed(format!("no {kind} key in the caller's custody")),
                    );
                    failed = true;
                    break;
                }
                Err(e) => {
                    registry.set_step(&run_id, step.position, StepOutcome::Failed(e));
                    failed = true;
                    break;
                }
            };
            let provider = provider_factory.build(kind, &key);
            let request = ChatRequest::new(step.model.as_str()).user(payload_to_prompt(&payload));
            match provider.complete(&request) {
                Ok(response) => {
                    // Pipe the assistant text onward as the next step's input.
                    payload = JsonValue::String(response.content);
                    registry.set_step(&run_id, step.position, StepOutcome::Completed);
                }
                Err(e) => {
                    registry.set_step(&run_id, step.position, StepOutcome::Failed(e.to_string()));
                    failed = true;
                    break;
                }
            }
        }
    }

    registry.set_run_status(
        &run_id,
        if failed {
            RunStatus::Failed
        } else {
            RunStatus::Completed
        },
    );
}

/// Resolve the caller's plaintext key for `kind` from custody, **by the caller's
/// `user_id`** (host-authoritative — never from the step). Returns `Ok(None)` when the
/// caller has saved no key for that provider. The first matching key is used.
fn resolve_provider_key(
    keys: &Arc<dyn ProviderKeyStore>,
    user_id: Uuid,
    kind: ProviderKind,
) -> Result<Option<String>, String> {
    let metas = keys.list(user_id).map_err(|e| e.to_string())?;
    let Some(meta) = metas.into_iter().find(|m| m.provider == kind) else {
        return Ok(None);
    };
    keys.get_secret(user_id, meta.id).map_err(|e| e.to_string())
}

/// Render the piped payload as an LLM prompt: a JSON string is used verbatim, `null`
/// is the empty prompt, anything else is its compact JSON form.
fn payload_to_prompt(payload: &JsonValue) -> String {
    match payload {
        JsonValue::String(text) => text.clone(),
        JsonValue::Null => String::new(),
        other => other.to_string(),
    }
}

/// Build an MCP tool's argument object from the step's static `tool_args` (an opaque
/// JSON object string, validated at create time) and the piped `payload`, which is
/// injected under `"input"`. A non-object/empty `tool_args` contributes no static
/// keys. Never includes a credential or an identity — those ride the minted cap.
fn build_tool_args(tool_args: &str, payload: &JsonValue) -> JsonValue {
    let mut map = match serde_json::from_str::<JsonValue>(tool_args) {
        Ok(JsonValue::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    map.insert("input".to_string(), payload.clone());
    JsonValue::Object(map)
}

// ── DTOs ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub(crate) struct LoopStepDto {
    position: i64,
    skill_name: String,
    scope_id: String,
    /// Provider kind for a provider step (P11.4); omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    /// Model id for a provider step; omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    /// External tool name for an MCP tool-call step (P11.5); omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool: Option<String>,
    /// Static tool arguments for an MCP tool step (parsed back to JSON); omitted
    /// otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_args: Option<JsonValue>,
}

#[derive(Debug, Serialize)]
pub(crate) struct LoopDto {
    loop_id: String,
    name: String,
    owner: String,
    created_at: String,
    steps: Vec<LoopStepDto>,
}

impl LoopDto {
    fn from_parts(record: LoopRecord, steps: Vec<LoopStepRecord>) -> Self {
        LoopDto {
            loop_id: record.loop_id,
            name: record.name,
            owner: record.owner,
            created_at: record.created_at,
            steps: steps
                .into_iter()
                .map(|s| LoopStepDto {
                    position: s.position,
                    skill_name: s.skill_name,
                    scope_id: s.scope_id,
                    provider: non_empty(s.provider),
                    model: non_empty(s.model),
                    tool: non_empty(s.tool),
                    tool_args: tool_args_json(&s.tool_args),
                })
                .collect(),
        }
    }
}

/// `Some(s)` unless `s` is empty (a non-matching step kind stores the empty string).
fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Parse a stored `tool_args` string back to JSON for the DTO. Empty (no args, or not
/// an MCP step) → `None`; a stored value is parsed (it was validated at create time).
fn tool_args_json(tool_args: &str) -> Option<JsonValue> {
    if tool_args.is_empty() {
        None
    } else {
        serde_json::from_str(tool_args).ok()
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct RunStepDto {
    position: i64,
    skill_name: String,
    scope_id: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    /// Provider kind for a provider step (P11.4); omitted otherwise. Lets the run view
    /// badge each step by kind without re-reading the loop definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    /// Model id for a provider step; omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    /// External tool name for an MCP tool-call step (P11.5); omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RunDto {
    run_id: String,
    loop_id: String,
    started_at: String,
    status: &'static str,
    steps: Vec<RunStepDto>,
}

impl From<RunState> for RunDto {
    fn from(run: RunState) -> Self {
        RunDto {
            run_id: run.run_id,
            loop_id: run.loop_id,
            started_at: run.started_at,
            status: match run.status {
                RunStatus::Running => "running",
                RunStatus::Completed => "completed",
                RunStatus::Failed => "failed",
            },
            steps: run
                .steps
                .into_iter()
                .map(|s| {
                    let (status, detail) = match s.status {
                        StepOutcome::Pending => ("pending", None),
                        StepOutcome::Running => ("running", None),
                        StepOutcome::Completed => ("completed", None),
                        StepOutcome::Failed(m) => ("failed", Some(m)),
                        StepOutcome::Denied(m) => ("denied", Some(m)),
                        StepOutcome::TimedOut => ("timed_out", None),
                    };
                    RunStepDto {
                        position: s.position,
                        skill_name: s.skill_name,
                        scope_id: s.scope_id,
                        status,
                        detail,
                        provider: non_empty(s.provider),
                        model: non_empty(s.model),
                        tool: non_empty(s.tool),
                    }
                })
                .collect(),
        }
    }
}

// ── Request bodies ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct CreateLoopBody {
    name: String,
    #[serde(default)]
    steps: Vec<StepInput>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StepInput {
    skill_name: String,
    scope_id: String,
    /// Optional provider selection (P11.4) — present makes this a *provider step*.
    /// Carries the model only; never a credential.
    #[serde(default)]
    provider_ref: Option<ProviderRefInput>,
    /// Optional MCP tool selection (P11.5) — present makes this an *MCP tool-call
    /// step*. Names the tool + args only; never a credential or an identity. A step
    /// is at most one kind — setting both `provider_ref` and `tool_ref` is rejected.
    #[serde(default)]
    tool_ref: Option<ToolRefInput>,
}

/// A provider step's model selection. **No key field** — the host resolves the key
/// from custody by the caller's identity at run time (ADR-0019).
#[derive(Debug, Deserialize)]
pub(crate) struct ProviderRefInput {
    /// Provider kind token (`"anthropic"` | `"openai"` | `"cerebras"`).
    provider: String,
    /// Model id (e.g. `"claude-opus-4-8"`).
    model: String,
}

/// An MCP tool step's selection (P11.5). **No identity/credential field** — the host
/// mints a caller-bound capability at run time; the step names only the tool + args
/// the scope authorizes (ADR-0020, probe P8.3).
#[derive(Debug, Deserialize)]
pub(crate) struct ToolRefInput {
    /// External tool name (e.g. `"web.search"`). Opaque to the core; the bridge routes it.
    tool: String,
    /// Optional static arguments — a JSON object the run engine merges with the piped
    /// payload (under `"input"`). Must be an object when present.
    #[serde(default)]
    args: Option<JsonValue>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct RunLoopBody {
    /// Optional input piped to the first step (default `null`).
    #[serde(default)]
    input: JsonValue,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// `POST /me/loops` — create a loop owned by the caller.
pub(crate) async fn create_loop_handler(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Json(body): Json<CreateLoopBody>,
) -> Result<Json<LoopDto>, ApiError> {
    require_clearance(&ctx, MANAGE_LOOPS_CLEARANCE)?;
    // Validate each step's optional kind selection up front. A step is at most one
    // kind: guest (neither ref), provider (`provider_ref`), or MCP tool (`tool_ref`).
    for step in &body.steps {
        if step.provider_ref.is_some() && step.tool_ref.is_some() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "a step is guest, provider, or mcp-tool — set at most one of provider_ref/tool_ref",
            ));
        }
        // A provider_ref must name a known kind and a non-empty model (the step picks
        // the model only; the key is resolved host-side).
        if let Some(p) = &step.provider_ref {
            if provider_runtime::parse_provider(&p.provider).is_none() {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    format!("unknown provider {}", p.provider),
                ));
            }
            if p.model.trim().is_empty() {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "a provider step requires a model",
                ));
            }
        }
        // A tool_ref must name a non-empty tool; any static args must be a JSON object
        // (the run engine merges the piped payload into it under `"input"`).
        if let Some(t) = &step.tool_ref {
            if t.tool.trim().is_empty() {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "an mcp-tool step requires a tool",
                ));
            }
            if t.args.as_ref().is_some_and(|a| !a.is_object()) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "tool args must be a JSON object",
                ));
            }
        }
    }
    let specs: Vec<LoopStepSpec> = body
        .steps
        .into_iter()
        .map(|s| {
            let (provider, model) = match s.provider_ref {
                Some(p) => (p.provider, p.model),
                None => (String::new(), String::new()),
            };
            let (tool, tool_args) = match s.tool_ref {
                Some(t) => (t.tool, t.args.map(|v| v.to_string()).unwrap_or_default()),
                None => (String::new(), String::new()),
            };
            LoopStepSpec {
                skill_name: s.skill_name,
                scope_id: s.scope_id,
                provider,
                model,
                tool,
                tool_args,
            }
        })
        .collect();
    let record = create_loop(&state.store, &ctx.email, &body.name, &specs)?;
    AuditLog::new(&state.store).record(&ctx, &format!("loop:create:{}", record.loop_id))?;
    let steps = loop_steps(&state.store, &record.loop_id)?;
    Ok(Json(LoopDto::from_parts(record, steps)))
}

/// `GET /me/loops` — the caller's own loops.
pub(crate) async fn list_loops_handler(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
) -> Result<Json<Vec<LoopDto>>, ApiError> {
    require_clearance(&ctx, MANAGE_LOOPS_CLEARANCE)?;
    let mut out = Vec::new();
    for record in list_loops_for_owner(&state.store, &ctx.email)? {
        let steps = loop_steps(&state.store, &record.loop_id)?;
        out.push(LoopDto::from_parts(record, steps));
    }
    Ok(Json(out))
}

/// `GET /me/loops/{id}` — read one of the caller's loops.
pub(crate) async fn get_loop_handler(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Path(id): Path<String>,
) -> Result<Json<LoopDto>, ApiError> {
    require_clearance(&ctx, MANAGE_LOOPS_CLEARANCE)?;
    let record = get_loop(&state.store, &id)?.ok_or_else(|| not_found_loop(&id))?;
    if record.owner != ctx.email {
        return Err(forbidden("not your loop"));
    }
    let steps = loop_steps(&state.store, &id)?;
    Ok(Json(LoopDto::from_parts(record, steps)))
}

/// `POST /me/loops/{id}/run` — compile + run the loop. Returns a `run_id`; poll
/// `GET /me/loops/runs/{id}` for live per-step status. Owner-gated; each step is
/// authorized at run time.
pub(crate) async fn run_loop_handler(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Path(id): Path<String>,
    Json(body): Json<RunLoopBody>,
) -> Result<Json<RunDto>, ApiError> {
    require_clearance(&ctx, MANAGE_LOOPS_CLEARANCE)?;
    let record = get_loop(&state.store, &id)?.ok_or_else(|| not_found_loop(&id))?;
    if record.owner != ctx.email {
        return Err(forbidden("not your loop"));
    }
    let steps = loop_steps(&state.store, &id)?;
    if steps.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "loop has no steps to run",
        ));
    }

    // Resolve the host base scope once; the executor clones it per step (the run
    // gate needs a `base: impl VisibilityScope` and consumes it each call).
    let base = ClearanceScope::resolve(&state.store, &ctx)?;
    let run_id = Uuid::new_v4().to_string();
    state
        .loop_runs
        .insert(RunState::new(&run_id, &id, &ctx.email, &steps));
    AuditLog::new(&state.store).record(&ctx, &format!("loop:run:{id}:{run_id}"))?;

    // Spawn the sequential executor (detached). It updates the shared registry as it
    // authorizes + runs each step; a std thread keeps the blocking waits off the
    // async runtime (the same model the Scheduler itself uses).
    let store = state.store.clone();
    let scheduler = state.scheduler.clone();
    let registry = state.loop_runs.clone();
    let provider_keys = state.provider_keys.clone();
    let provider_factory = state.provider_factory.clone();
    let caps = state.caps.clone();
    let mcp_bridge = state.mcp_bridge.clone();
    let caller = ctx.clone();
    let exec_run_id = run_id.clone();
    let input = body.input;
    std::thread::spawn(move || {
        execute_loop(
            store,
            scheduler,
            registry,
            provider_keys,
            provider_factory,
            caps,
            mcp_bridge,
            exec_run_id,
            caller,
            base,
            steps,
            input,
        );
    });

    let run = state
        .loop_runs
        .snapshot(&run_id)
        .expect("run was just inserted");
    Ok(Json(RunDto::from(run)))
}

/// `GET /me/loops/runs/{id}` — the live per-step status of a run the caller started.
pub(crate) async fn get_run_handler(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Path(run_id): Path<String>,
) -> Result<Json<RunDto>, ApiError> {
    require_clearance(&ctx, MANAGE_LOOPS_CLEARANCE)?;
    let run = state
        .loop_runs
        .snapshot(&run_id)
        .ok_or_else(|| not_found_run(&run_id))?;
    if run.caller != ctx.email {
        return Err(forbidden("not your run"));
    }
    Ok(Json(RunDto::from(run)))
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn forbidden(message: &'static str) -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "forbidden", message)
}

fn not_found_loop(id: &str) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", format!("loop {id}"))
}

fn not_found_run(run_id: &str) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "not_found",
        format!("loop run {run_id}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_evicts_oldest_beyond_cap() {
        let reg = LoopRunRegistry::new();
        for i in 0..(MAX_RETAINED_RUNS + 5) {
            reg.insert(RunState::new(&format!("run-{i}"), "loop", "caller", &[]));
        }
        // The five oldest were evicted (FIFO); the rest, including the newest, remain.
        assert!(reg.snapshot("run-0").is_none());
        assert!(reg.snapshot("run-4").is_none());
        assert!(reg.snapshot("run-5").is_some());
        assert!(reg
            .snapshot(&format!("run-{}", MAX_RETAINED_RUNS + 4))
            .is_some());
    }

    #[test]
    fn reinserting_the_same_run_id_does_not_grow_order() {
        let reg = LoopRunRegistry::new();
        // Same id inserted MAX+50 times must not evict itself or unbound `order`.
        for _ in 0..(MAX_RETAINED_RUNS + 50) {
            reg.insert(RunState::new("same", "loop", "caller", &[]));
        }
        assert!(reg.snapshot("same").is_some());
    }
}
