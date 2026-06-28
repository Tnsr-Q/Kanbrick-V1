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
//! 2. on authorization, schedules the skill's `guest` on the `Scheduler` under the
//!    host-authoritative caller context and waits for its terminal `TaskStatus`,
//!    piping each step's output payload into the next step's input.
//!
//! `GET /me/loops/runs/{id}` reports the per-step status live. Run history is kept
//! **in-process** for now (the [`LoopRunRegistry`]); persisting it so it survives a
//! restart is P11.5. Per-step provider/key selection is P11.4, external MCP tool
//! steps are P11.5 — this slice is **guest-step loops only**.
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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use kanbrick_auth::{require_clearance, AuditLog, ClearanceScope};
use kanbrick_core::abi::GuestRequest;
use kanbrick_core::{ClearanceLevel, FirmContext};
use kanbrick_discovery::ScopeGrants;
use kanbrick_mesh::{RetryPolicy, Scheduler, TaskStatus};
use kanbrick_store::{
    create_loop, get_loop, list_loops_for_owner, loop_steps, read_guest_policy, LoopRecord,
    LoopStepRecord, LoopStepSpec, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::{ApiError, AppState, AuthedContext};

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

// ── In-process run registry (durable run history is P11.5) ───────────────────

/// Live, in-process state of every loop run on this node. Cheaply cloneable (the
/// map is behind an `Arc<Mutex<…>>`), so it rides in [`AppState`].
#[derive(Clone, Default)]
pub struct LoopRunRegistry {
    runs: Arc<Mutex<HashMap<String, RunState>>>,
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

    fn insert(&self, run: RunState) {
        self.runs
            .lock()
            .expect("loop-run lock")
            .insert(run.run_id.clone(), run);
    }

    fn snapshot(&self, run_id: &str) -> Option<RunState> {
        self.runs
            .lock()
            .expect("loop-run lock")
            .get(run_id)
            .cloned()
    }

    fn set_step(&self, run_id: &str, position: i64, outcome: StepOutcome) {
        let mut runs = self.runs.lock().expect("loop-run lock");
        if let Some(run) = runs.get_mut(run_id) {
            if let Some(step) = run.steps.iter_mut().find(|s| s.position == position) {
                step.status = outcome;
            }
        }
    }

    fn set_run_status(&self, run_id: &str, status: RunStatus) {
        let mut runs = self.runs.lock().expect("loop-run lock");
        if let Some(run) = runs.get_mut(run_id) {
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
                })
                .collect(),
        }
    }
}

// ── The compiler / executor ──────────────────────────────────────────────────

/// Execute a loop's steps sequentially, gating each through `authorize_skill` and
/// running the authorized guest on the `Scheduler`. Runs on a background thread; it
/// communicates progress only through the shared [`LoopRunRegistry`].
// The executor inherently carries the full run spec — the engine handles (store,
// scheduler, registry) plus the run's identity, base scope, steps, and input; a
// context struct would only relocate the arity. Mirrors the `Scheduler` trigger fns.
#[allow(clippy::too_many_arguments)]
fn execute_loop(
    store: Arc<Store>,
    scheduler: Arc<Scheduler>,
    registry: LoopRunRegistry,
    run_id: String,
    caller: FirmContext,
    base: ClearanceScope,
    steps: Vec<LoopStepRecord>,
    initial_input: JsonValue,
) {
    let grants = ScopeGrants::new(&store);
    let mut payload = initial_input;
    let mut failed = false;

    for step in &steps {
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

        // Defense-in-depth: the loop path must enforce the same guest clearance floor
        // as `POST /guests/{name}`. `authorize_skill` checked the *skill*'s declared
        // clearance; also require the caller to meet the backing *guest*'s policy
        // floor, so a skill that under-declares its guest's clearance can't reach a
        // higher-floor guest through a loop. (Each guest self-enforces too, but this
        // keeps the floor uniform across both paths and fails closed at the gate.)
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
            // Policy satisfied, or unknown guest (the scheduler reports GuestNotFound).
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

// ── DTOs ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub(crate) struct LoopStepDto {
    position: i64,
    skill_name: String,
    scope_id: String,
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
                })
                .collect(),
        }
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
    let specs: Vec<LoopStepSpec> = body
        .steps
        .into_iter()
        .map(|s| LoopStepSpec {
            skill_name: s.skill_name,
            scope_id: s.scope_id,
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
    let caller = ctx.clone();
    let exec_run_id = run_id.clone();
    let input = body.input;
    std::thread::spawn(move || {
        execute_loop(
            store,
            scheduler,
            registry,
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
