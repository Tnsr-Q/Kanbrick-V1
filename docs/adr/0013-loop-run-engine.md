# ADR 0013 — Loop run engine: (:Loop)/(:LoopStep) compiled onto the mesh Scheduler

- **Status:** Accepted
- **Date:** 2026-06-28
- **Context:** Phase 11 (Skill/Loop Ecosystem), slice **P11.3** — the "it runs"
  milestone. Builds directly on the skill model (ADR-0012), the per-scope `Skill`
  primitive and its run gate `ScopeGrants::authorize_skill` (`kanbrick-discovery`
  `grants.rs`), the **existing** mesh `Scheduler` (`kanbrick-mesh` `scheduler.rs`,
  #25/#26), the SparrowDB write dialect (ADR-0001/0006), and host-authoritative
  identity (ADR-0002/0016).
- **Deciders:** P11 agent + **operator** (the loop schema + the "thin compiler onto
  the existing Scheduler, no new run fabric" shape are one-way doors; the operator
  chose walking-skeleton-first this session).

## Context

ADR-0012 fixed what a *skill* is (a versioned, grant-gated wrapper over a WASM
guest) and P11.2/P11.2b exposed the grant lifecycle and the skill ⇄ scope bridge
over HTTP. What was still missing is the thing the ecosystem exists for: an employee
composing skills into a **loop** and **running it**.

The two hardest pieces already exist and are tested:

- the **run engine** — `kanbrick-mesh::Scheduler` (`schedule_with_retry` / `status`
  / `wait`, per-guest concurrency, wall-clock timeouts via epoch interruption); and
- the **run gate** — `ScopeGrants::authorize_skill(caller, base, scope_id,
  skill_name, now) → (Skill, ProjectScope)`, which enforces an ACTIVE+unexpired
  scope, the caller being the grantee, and the caller's clearance meeting the
  skill's floor.

So P11.3 is **wiring**, not a new runtime. The load-bearing decisions are the loop
*schema*, where the run gate sits, and how a loop *compiles* onto the Scheduler.

## Decision

1. **Schema — `(:Loop)` + `(:LoopStep)`.** A loop is an owned, ordered pipeline:
   `(:Loop {loop_id, name, owner, created_at})` linked by `[:HAS_STEP]` to
   `(:LoopStep {step_id, loop_id, position, skill_name, scope_id})`. Each step names
   a **skill** and the **scope** it runs under — the step is the polymorphic unit
   (guest-backed now; provider/LLM steps are P11.4, external MCP tool steps are
   P11.5, so this slice is **guest-step loops only**). Persistence lives in
   `kanbrick-store::loop_registry` and follows the ADR-0001 dialect verbatim from
   `skill_registry`: parameterized `MERGE` on the unique key + `MATCH … SET`, and
   the relationship `MERGE` on the **non-parameterized** inline-escaped path
   (SPA-233). This persists the loop **definition** only.

2. **The run gate is per-step, at run time.** Running a loop authorizes **each
   step** through `authorize_skill` immediately before it executes — this is the
   runtime gate deferred from P11.2b. The host builds the caller's base visibility
   with `ClearanceScope::resolve(store, &ctx)` and passes it in (the gate composes
   the additive grant onto it). A rejected step marks the step **denied** and stops
   the run. Authorization is just-in-time rather than a single upfront pass, so a
   scope that expires mid-loop stops the loop at the next step. Identity is never
   taken from a payload: the `caller` is the validated `FirmContext`, and
   `authorize_skill` audits each authorized step under it. As **defense-in-depth**,
   the executor additionally requires the caller to meet the backing **guest's own
   policy floor** (`read_guest_policy`) before scheduling — the same floor
   `POST /guests/{name}` enforces — so the loop path is never a weaker door than the
   direct guest path, even if a skill under-declares its guest's clearance or a
   future guest forgets its internal check.

3. **The loop *compiles* onto the Scheduler — it adds no run fabric.** An authorized
   step becomes one `Scheduler::schedule_with_retry(skill.guest, caller, request, …)`
   plus a `wait` for its terminal `TaskStatus`; each step's output payload is piped
   into the next step's input. The run is driven by a single background thread (the
   same threading model the Scheduler uses), so the HTTP request returns a `run_id`
   immediately and the run progresses off the request path. The `Scheduler` is held
   in `AppState`, wrapping the same `MeshRuntime` the synchronous `/guests/{name}`
   path uses.

4. **Run history is in-process for this slice.** `POST /me/loops`,
   `POST /me/loops/{id}/run`, and `GET /me/loops/runs/{id}` are the surface; the run
   state (per-step status mirroring `TaskStatus`, plus `denied`/`pending`) lives in
   an in-process registry. **Persisting run history so it survives a restart is
   P11.5** — the loop *definition* is already durable (point 1), which is what the
   handoff's "persist the loop schema" calls for.

## Consequences

- **It runs, on the real gate.** A loop of guest steps executes through
  `authorize_skill` and the Scheduler — the walking-skeleton milestone — with the
  thin run-and-watch UI (P11.7) reading `GET /me/loops/runs/{id}`.
- **Ownership vs. authorization are separate bars.** Loop *ownership* gates who may
  run/read a loop (the handler); per-step *authorization* gates whether each step
  may execute (the run gate). Typically the loop owner is the grantee of every
  step's scope; a step on a scope the caller does not own is denied at run time.
- **The composed `ProjectScope` is not yet applied inside a guest.** `authorize_skill`
  returns `(Skill, ProjectScope)`; the run *gate* is the authorization itself, and
  the skill's `guest` is what we schedule. Threading the composed additive-grant
  visibility into the guest's own graph queries (today filtered by the caller's base
  clearance) needs a mesh seam to inject a `VisibilityScope` into an invocation —
  tracked as later work, not part of the walking skeleton. The security control here
  is the gate (a step runs only if `authorize_skill` returns `Ok`).
- **Loops are sequential, run-once pipelines for now.** Iteration / `exit_condition`
  / `max_iters` and event-chained steps (`emit`/`on_event`, which the Scheduler
  already supports) are deferred; the schema leaves room for them without a rewrite.
- **Deferred, by design:** durable run history (P11.5), per-step provider/key
  injection (P11.4), external MCP tool-call steps (P11.5), and richer loop-builder UI
  (P11.6/P11.7).
