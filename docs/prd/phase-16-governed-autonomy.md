# PRD — Phase 16 "Governed Autonomy": the loop, not just the loop body

- **Status:** Hardened draft for operator review (this PR)
- **Date:** 2026-07-02
- **Inputs:** the operator's Loopfleet assessment (loop-engineering / fleet-engineering
  → Kanbrick mapping); a six-subsystem code verification pass over this repo (every
  claim below is grounded in source, cited by file + symbol); ADR-0012/0013/0019/0020/0021;
  epics #82 (Phase 11) and #83 (Phase 12).
- **Scope discipline:** this phase ships **no new skills, no new agents, no new tools,
  no new guests, and no new run fabric** (ADR-0013 one-way door). Every slice is
  recurrence, governance, or evidence — wiring and enforcing what already exists.

---

## 0. Thesis

**Kanbrick has the loop body; it does not yet have the loop.**

Phase 11 shipped a complete, host-enforced execution pipeline: author a SKILL.md →
publish to a versioned registry (author-pinned, dual-gate lead review, ADR-0021) →
bind onto a grant-gated scope → compose a `(:Loop)` of polymorphic steps
(guest XOR provider XOR mcp-tool) → run with **every step** gated just-in-time by
`ScopeGrants::authorize_skill` on the mesh `Scheduler` → watch live in the Cockpit.
Neither reference repo (loop-engineering, fleet-engineering) has the per-step
authorize gate or the dual-gate publish review as *enforced mechanisms* — Kanbrick
crossed from convention to enforcement at both the loop and fleet layers.

But in loop-engineering's frame, the recurrence **is** the loop; the pipeline is just
the body. Today every run is human-triggered, run-once, and its results evaporate:
run history lives in a 512-entry in-process FIFO lost on restart
(`kanbrick-api/src/loops.rs`, `LoopRunRegistry`), and the run's final payload is
discarded entirely.

The dangerous asymmetry: the mesh already ships tested recurrence primitives —
`Scheduler::schedule_interval` / `schedule_on_event` with cancellable
`TriggerHandle` (`kanbrick-mesh/src/scheduler.rs`) — **wired to nothing**. Cadence
is the cheapest change on the board (one call site) and the only one that changes
the risk class of the whole system, and *nothing in the tracker gates it*: no epic,
slice, or ADR mentions loop recurrence, a kill switch, or unattended execution.
This PRD exists to make the cheap change impossible to make ungoverned.

---

## 1. Verified ground truth (what changed from the draft assessment)

The full claim-by-claim verdict table is in Appendix A. Three corrections are
load-bearing enough to state up front, because slices hang on them:

### 1.1 The guest graph channel is a live write hole (draft claim C7 was wrong in both words)

The draft said guests "can only **propose writes** through audited host imports."
In fact there are no propose semantics, and the channel is not read-only:
`GuardedStore::query_graph` (`kanbrick-auth/src/guarded.rs`) audits the query and
clearance-filters **returned rows**, but passes the guest's arbitrary Cypher to
`Store::query` → `Store::execute_with` (`kanbrick-store/src/store.rs`) — the same
execution path as host writes. A guest can pass `MERGE` / `MATCH … SET` and it
**commits directly against the firm graph**, before and regardless of row
filtering. The documented security model ("guests read the graph only through
audited, clearance-filtered host imports" — README, docs/SECURITY.md) describes
reads only; writes were never in the contract, so this is a defect to seal, not a
behavior to preserve. The three business guests are read+emit only, so sealing is
non-breaking. **Verified first-hand, not just by the fan-out.**

### 1.2 The audit trail cannot yet pass fleet-engineering's accountability test

The `(:AuditEntry)` node stores exactly: `entry_id`, `user_id`, `clearance`,
`query_hash` (SHA-256 of the query/marker text), `timestamp`
(`kanbrick-auth/src/audit.rs`). Consequences for autonomy:

- Even semantic markers (`loop:run:{id}:{run_id}`, `skill:review:approved:…`) are
  stored **hash-only** — the trail cannot be read back, only matched against a
  guessed string.
- No actor-origin attribution: a loop step executes under the owner's
  `FirmContext`, indistinguishable from the owner acting live.
- Step outcomes (failed/denied/timed-out) are never durably recorded; guest
  executions inside loop steps bypass the invocation audit entirely (the
  `Scheduler` writes no audit entries).
- There is no audit read surface anywhere (API, CLI, or Cockpit).

"Which agent did it, with what authority, against what task, evidenced by what?"
is answerable today for interactive requests, and **not answerable at all** for
anything a scheduler initiates. This must be fixed *before* cadence, not after.

### 1.3 Durable run history was deferred twice and never shipped

ADR-0013 deferred `(:LoopRun)`/`(:LoopRunStep)` persistence to "P11.5"; the P11.5
slice shipped MCP tool steps and re-deferred it (ADR-0020); it is still open.
Runs die with the process, in-flight runs die silently (no graceful shutdown —
bare `axum::serve`), and the final payload is dropped even while the run lives.

### 1.4 Smaller verified corrections

- **`kanbrick-loops` is not "early-stage"** (draft C8): the crate itself is the
  manifest parser by design; the run engine lives in `kanbrick-api/src/loops.rs`
  with three step kinds, per-step + whole-run wall-clock budgets (30s/300s),
  payload piping, and defense-in-depth guest-floor checks. (README's crate
  description is stale the other way — housekeeping note in §8.)
- **Grant expiry fails open**: `is_expired` treats an unparseable `expires_at` as
  *never expiring* (`kanbrick-discovery/src/grants.rs`, documented in its own doc
  comment). Tolerable when grants only widen reads; intolerable once autonomy
  rides grant expiry (§4). Verified first-hand.
- **`kanbrick-tokens` is depended on by nothing**; provider-step `Usage` is
  discarded on the floor in the loop executor. Budgets are entirely unenforced.
- **No kill switch of any kind**: repo-wide grep for pause/halt/disable-all finds
  zero mechanisms; the loop executor thread is detached and unaddressable; the
  Cockpit's "stop watching" cancels only the UI poll.
- **HITL surface is thinner than the draft assumed** (C4 partial): scope-grant
  approve/deny is API-only; the only approval UI is the skill-review queue.

---

## 2. Aligned design decisions (operator + agent, this session)

These were converged explicitly and are treated as settled inputs, not open
questions:

1. **Thesis framing** — §0 verbatim: ship the loop, not more body.
2. **Forced ordering** — the gaps are not a backlog, they have a dependency
   structure: *(draft state + report-only default) → (cadence + owner-of-record +
   kill switch, atomically) → (budgets, folded into #83) → (maker/checker, pulled
   only by promotion demand)*. Cadence ships **last of the preconditions, never
   first**.
3. **Envelope vs. operating point** — SKILL.md declares the skill's *capability
   envelope* (invariants reviewers attest to under ADR-0021: maximum autonomy the
   skill is safe at, whether it proposes graph changes or only reads); the
   `(:Loop)` node declares the *operating point* (cadence, actual autonomy,
   budget cap, escalation target). Loop policy never lives in the skill manifest
   (two loops sharing one skill must be able to differ), and the envelope never
   lives on the loop (reviewers, not binders, are equipped to judge a skill's
   internals). **Effective policy = min(envelope, operating point, grant)**,
   host-enforced at the existing gate. ADR-0021's review-status reset on every
   publish means widening an envelope automatically forces re-review — the
   envelope is re-attested by construction.
4. **Budget composition** — every loop run debits its **owner-of-record's**
   employee budget (the same principal whose provider keys ADR-0019 resolves),
   tagged with loop/run attribution; a per-loop cap is a *sub-limit within* the
   owner's envelope, never a separate pool. One enforcement mechanism (the P12.4
   sweep + a step-boundary hard stop) covers both axes. Owner-of-record is
   therefore triply load-bearing: accountability, economics, and key custody —
   promoting a loop to unattended is also consenting to one's keys being spent
   while absent, and the promotion grant must say so.
5. **Autonomy expires by default** — because authorization is just-in-time per
   step (ADR-0013), promotion riding a TTL'd scope grant means a lapsed grant
   halts the loop at its next step/tick with no new machinery. No perpetual
   unattended authority can exist.

One deliberate naming decision: loop-engineering's autonomy ladder is "L1/L2/L3",
which collides fatally with Kanbrick's L1–L5 *clearance* tiers. This PRD uses
**`report_only` → `checker_gated` → `unattended`** as the autonomy vocabulary.

---

## 3. Non-goals (this phase)

- No new skills, agents, tools, guests, or step kinds. Domain loops (portfolio
  triage, compliance sweep, valuation refresh) appear only as **acceptance demos
  composed from the three existing guests**.
- No new run fabric, no branching/parallel/conditional loop execution, no
  per-step retry policy changes (ADR-0013 constraints stand).
- No event-triggered loops (`schedule_on_event` stays unwired): the event bus is
  in-memory and non-durable; event-driven cadence without a durable bus is an
  accountability hole. Revisit after this phase.
- No threading of the composed `ProjectScope` into guest graph queries (the
  acknowledged ADR-0013 deferral stands; the gate remains the control).
- No fleet-repo tooling ports (`fleet-init`, JSON manifests, etc.) — Kanbrick's
  governance layer is already enforced infrastructure; we steal doctrine
  (checklists, failure-mode catalogs) as review rubrics only.
- No audit read/query UI beyond what P16.2 needs to prove evidence exists
  (a full audit browser is later work).

---

## 4. The autonomy model (target state)

### 4.1 Ladder

| Level | Meaning | Requires |
|---|---|---|
| `report_only` | Runs produce **proposals only**; a human reviews and applies. The default for every loop, forever, unless promoted. | Nothing beyond loop ownership (today's bar). |
| `checker_gated` | Proposals auto-apply **iff** the loop's bound verifier approves them; otherwise escalate to a human. | An approved, TTL'd autonomy grant (dual-gate, management chain over the owner) **and** a bound checker (P16.7). |
| `unattended` | Proposals auto-apply on success; failures/denials escalate. | An approved, TTL'd autonomy grant approved at **L5**. |

### 4.2 Where each fact lives

- **SKILL.md envelope** (new optional frontmatter keys, parsed by
  `kanbrick-loops`, attested under ADR-0021 review):
  `max_autonomy: report_only|checker_gated|unattended` (absent ⇒ `report_only`,
  fail-closed) and `effects: read|propose` (absent ⇒ `read`).
- **`(:Loop)` operating point** (schema additions to `loop_registry`):
  `autonomy` (requested level), `escalation` (who is notified on
  failure/denial/budget-stop — via the existing messenger), `enabled` flag,
  and per-loop budget cap (P16.6).
- **Authority**: an autonomy level above `report_only` is live only while an
  approved, unexpired **autonomy grant** exists for (loop, level). Reuses the
  scope-request dual-gate lifecycle (`eligible_grantor` chain / L5) with a
  **mandatory TTL** — this is the "promotion = a scope-grant approval in your
  existing flow" move, and grant lapse is the automatic sunset.
- **Enforcement point**: the run executor computes
  `effective = min(min over steps' skill envelopes, loop.autonomy, live grant level)`
  at run start **and re-checks the grant at each apply decision** — the same JIT
  shape as `authorize_skill`. This mirrors the existing defense-in-depth
  precedent exactly (the run path already takes the *max* of clearance floors;
  it now also takes the *min* of autonomy bounds).

### 4.3 Identity for unattended runs

No stored JWTs (8h TTL, unrevocable — not a viable principal). At each trigger
fire, the host constructs the owner's `FirmContext` **fresh from the store**
(current clearance and roles, synthetic `session_id` derived from the trigger),
exactly as host-authoritative as the login path — the host is the trigger, so no
token needs to exist. Consequences, all desirable: a clearance demotion takes
effect at the next tick; a deactivated owner auto-pauses the trigger; every
scheduled run has a real, current principal. Runs carry `origin:
manual|schedule` plus the trigger id, so the audit trail distinguishes the owner
acting from the owner's loop acting.

---

## 5. Slices

Ordering is normative. P16.1–P16.3 are preconditions for P16.5; **cadence may not
merge before them**.

### P16.1 [AFK] Seal the guest graph channel; fail-closed grant expiry — *ADR-0022*

- Enforce read-only Cypher on `GuardedStore::query_graph` (write-classifier or
  parse-level rejection; **fail closed** on anything unclassifiable). The three
  business guests are read+emit only, so this is non-breaking; add a regression
  test that a guest-submitted `MERGE`/`SET`/`CREATE`/`DELETE` is refused and
  audited as refused.
- Fix `is_expired` fail-open: blank stays non-expiring only if that is an
  explicit operator choice; an **unparseable** `expires_at` must read as expired
  (fail closed), with a store-level test.
- Verify (and fix if real) the reported revoke-path cache gap
  (`revoke_scope` passing `cache: None`, so revocation may not invalidate the
  discovery cache) — matters because §4's sunsets lean on revocation latency.
- **Why first:** every later slice ("guests propose, the host commits") is a lie
  while an unsanctioned write path exists, and autonomy-by-grant is unsafe while
  expiry can fail open.

### P16.2 [AFK] Durable runs + evidence — *the twice-deferred persistence, plus attribution*

- Persist `(:LoopRun)`/`(:LoopRunStep)` via the ADR-0001 dialect (the
  `loop_registry`/`skill_registry` template), written through the run lifecycle,
  including the **final payload** (currently discarded) and terminal step
  outcomes (`failed`/`denied`/`timed_out` with reasons).
- `(:LoopRun)` snapshots its authority context: `origin` (`manual` now;
  `schedule` arrives in P16.5), owner, effective autonomy level, and the grant
  ids that authorized it — the fleet accountability test becomes answerable from
  one node's neighborhood.
- Lifecycle audit events (`loop:*`, `skill:*`, review actions) gain a
  **structured plaintext action field** on `(:AuditEntry)`; graph-query entries
  keep hash-only privacy. (The dual-record split is the pre-made decision;
  operator may veto in review.)
- Mark orphaned runs (`interrupted`) at boot; keep the in-process registry as
  the hot cache. Includes a small spike proving SparrowDB write-ordering
  assumptions for run-then-steps persistence (transactionality is unverified —
  flagged risk).

### P16.3 [AFK] Proposals: draft-vs-committed — *the worktree analogue, ADR-0022 companion*

- A run's output lands as a host-written `(:Proposal
  {proposal_id, run_id, loop_id, owner, status: draft|applied|rejected, payload}
  )`. **The host is the graph's only writer; guests never gain a write path.**
- Two proposal kinds: **report** (the payload *is* the deliverable — triage
  summaries, drafted memos; "apply" = acknowledge) and **change-set** (a
  constrained, schema-validated list of graph mutations the host applies through
  `GuardedStore` under the applier's clearance — never raw Cypher from a guest).
  A skill whose manifest declares `effects: propose` emits change-sets; `read`
  skills emit reports.
- `POST /me/proposals/{id}/apply|reject`, `GET /me/proposals?loop=…`; audited;
  escalation notification to the loop's `escalation` target via the existing
  messenger on creation. **Every loop is `report_only` from the day this slice
  merges** — the L1-default lands here, before any recurrence exists.

### P16.4 [HITL] Autonomy ladder: envelope, operating point, promotion grants — *ADR-0023*

- The §4 model: SKILL.md envelope keys (+ parser + fail-closed defaults),
  `(:Loop)` operating-point fields, autonomy grants on the scope-request
  dual-gate with mandatory TTL, three-way `min` enforced in the executor, and
  re-check at each apply decision.
- HITL decisions for the operator inside this slice: approver bar for
  `unattended` (proposed: L5 only), TTL ceilings per level, whether
  `checker_gated` additionally requires N clean `report_only` runs.

### P16.5 [HITL] Cadence + owner-of-record + kill switch — *atomic slice, ADR-0024*

These three ship in **one slice** because a cron-triggered run without an
accountable principal, and a scheduler that can fire without a human present but
cannot be stopped, are each standalone regressions.

- **Cadence:** durable `(:LoopTrigger {trigger_id, loop_id, interval, enabled,
  created_by})`, replayed onto `Scheduler`-style interval firing at boot
  (in-process triggers are lost on restart today by design). Trigger
  creation/pause is loop-owner + TTL'd like any grant; each fire runs the loop
  exactly as `POST /me/loops/{id}/run` does, under the §4.3 host-constructed
  owner context, `origin: schedule`.
- **Kill switch, three tiers, all audited:**
  - *Sovereign:* persisted global pause (survives restart; if paused at boot,
    boots paused) + in-memory flag checked at trigger fire, at every
    `execute_loop` step boundary, and at the mesh invoke choke point
    (`invoke_with_deadline_cap`). L5-only `POST /admin/runtime/pause|resume`.
  - *Per-loop:* the `(:Loop).enabled` flag (owner or L5).
  - *Per-run:* a cancel flag in the run registry checked at step boundaries;
    `POST /me/loops/runs/{id}/cancel`.
  - Documented stop-latency bound: one step boundary (≤ ~35s worst case with
    the current step timeout). Scope-revocation is **not** the kill switch
    (it gates the *next* authorization and has cache-latency caveats — P16.1).
- **Restart honesty:** graceful-shutdown drain for in-flight runs
  (`with_graceful_shutdown`), plus the P16.2 `interrupted` marking as the
  backstop.

### P16.6 [AFK — amends epic #83, does not fork it] Loop attribution + hard stop in the token ledger

- `kanbrick-api` takes its **first dependency on `kanbrick-tokens`**; provider
  steps record `Usage` (today discarded) as ledger rows extended with
  `loop_id/run_id/step` dimensions, debited to the **owner-of-record**.
- Per-loop cap = sub-limit within the owner's envelope; **token-denominated
  now** (enforceable immediately), USD-denominated once P12.1/P12.2 price the
  rows — the cap field carries its unit.
- Enforcement: pre-flight at run start (owner envelope + loop cap not
  exhausted) and a **hard stop at each step boundary** (`budget_exhausted`
  terminal state → escalation). The P12.4 sweep then covers both axes with one
  mechanism; P12.3's approval flow gains "approve a loop cap = approve a
  carve-out of an employee budget," not a new budget type.

### P16.7 [AFK mechanism; pull-based] Maker/checker as the promotion gate — *not a general feature*

- A `(:Loop)` may bind a **verifier**: a (skill, scope) pair invoked by the host
  with the maker's proposal as input, returning approve/reject + reason.
  Verifier verdict gates auto-apply at `checker_gated`; at `report_only` it is
  advisory annotation on the proposal.
- Ships the mechanism + a test-fixture verifier only. Real domain checkers are
  new skills — **out of scope by this PRD's own rule**; the slice is pulled only
  when a real `checker_gated` promotion is demanded.

### P16.8 [AFK, thin] Cockpit: the operator surfaces the above already imply

- Proposals inbox (review/apply/reject), the kill-switch control (L5), autonomy
  + trigger badges on LoopRunner, and — closing the verified C4 gap — the
  scope/autonomy **grant approval inbox** (approve/deny already exist as
  routes; there is no UI). Reuses the P11.7/P11.8 patterns verbatim; no new
  ADR.

### Dependency DAG

```
P16.1 ──► P16.3 ──┬──► P16.5 ◄── P16.4
P16.2 ────────────┘        │
P16.4 ──► P16.7 (pull)     └──► P16.6 (amends #83; token-caps immediately,
                                        USD caps after P12.1/P12.2)
```

---

## 6. Acceptance demos (existing guests only — these are tests, not deliverables)

1. **Daily portfolio triage** — reporting guest skill, daily trigger,
   `report_only`: a proposal (report) appears each morning; owner notified via
   messenger; nothing commits without a human. Proves P16.2/3/5.
2. **Compliance sweep** — compliance guest skill, cadence, `report_only` with
   `escalation` set to an L4: violations escalate rather than auto-fix
   (compliance loops should likely *never* leave `report_only`; its SKILL.md
   envelope can pin `max_autonomy: report_only` — proving the envelope has
   teeth). Proves P16.4.
3. **Valuation refresh** — valuation guest skill with `effects: propose`
   emitting a change-set proposal; demo `checker_gated` apply with the fixture
   verifier; demo budget hard-stop by setting a 1-token cap. Proves P16.3/6/7.
4. **Sovereign stop** — with all three demo triggers armed, L5 hits pause: no
   trigger fires, in-flight runs stop at the next step boundary, everything is
   audited, and the state survives a restart. Proves P16.5.

---

## 7. Open questions for the operator (decide during P16.4/P16.5 review)

1. Audit dual-record split (plaintext lifecycle actions vs. hash-only queries) —
   privacy stance confirmation. (Pre-made in P16.2; veto-able.)
2. Approver bar for `unattended` (proposed L5-only) and TTL ceilings per level.
3. Blank `expires_at` semantics after the fail-closed fix: keep "never expires"
   for interactive grants, or require explicit TTLs everywhere?
4. SparrowDB multi-statement atomicity — if the P16.2 spike finds no ordering
   guarantee, run/step persistence adopts write-ahead ordering (steps before
   run-terminal marker), and the ADR must say so.
5. Trigger floor/ceiling for cadence (minimum interval; proposed ≥ 5 min) to
   bound runaway spend and log growth.

---

## 8. Housekeeping (ride along, no slice needed)

- README `kanbrick-loops` description and the tracker's P11.8 "in flight" row
  are stale (P11.8 is merged; the crate blurb undersells the run engine).
- The loop-engineering / fleet-engineering doctrine artifacts worth vendoring as
  **review rubrics** (not code): the loop-design checklist and anti-patterns
  and failure-mode catalogs → `docs/agents/` as ADR-review aids.

---

## Appendix A — Claim verification (operator's draft assessment vs. code)

Six parallel subsystem readers + an adversarial critic pass; contradictions
resolved by direct code reads. Verdicts: **confirmed** (true as stated),
**partial** (kernel true, materially incomplete/outdated), **refuted**.

| # | Draft claim | Verdict | What the code says |
|---|---|---|---|
| C1 | Content-addressed guest registry + skill publish/browse/review/bind endpoints | **confirmed** | SHA-256 asset store, L5-only upload/activate, compile-then-swap hot reload; full skill lifecycle incl. ADR-0021 trust gate (`kanbrick-api/src/skills.rs`). |
| C2 | JWT/Argon2id; host-authoritative `FirmContext`, never trusted from payloads | **confirmed** | No context setter exists in the ABI; guests get read-only ctx imports; executor split preserves it via minted capabilities. |
| C3 | Five-tier clearance; scope request/approval/revocation endpoints | **confirmed** | `ScopeGrants` dual-gate (`eligible_grantor`), full HTTP lifecycle. Caveat: expiry fail-open bug (§1.4, fixed in P16.1). |
| C4 | Scope approval flow + Cockpit HITL surfaces "partially there" | **partial** | Flow yes; Cockpit surface exists only for skill reviews. Grant approve/deny is API-only — no inbox UI (P16.8). |
| C5 | Single audited, clearance-filtered read choke point; invocation auditing | **partial** | The choke point is real (`GuardedStore`). But audit is hash-only, has no read surface, loop-step guest executions bypass invocation audit, and origin attribution doesn't exist (§1.2). |
| C6 | `kanbrick-tokens` = token ledger/budgeting | **partial** | Real pricing/ledger/budget *types* (nano-USD, per-user, in-memory) — depended on by nothing; no routes; `Usage` discarded in the loop path. |
| C7 | Sandboxed Wasmtime/WASI guests; guests only *propose* writes through audited imports | **partial** | Sandbox half fully confirmed (fresh Store, no FS/net/stdio, 64 MiB, fuel, epoch deadlines, 5-import ABI). Write half wrong: no propose semantics, and the query import is a live write channel (§1.1). |
| C8 | `kanbrick-loops` early-stage: manifest parsing + create/run/history endpoints | **partial** | Crate = parser by design; the *system* is a working 3-step-kind engine with per-step gating (§1.4). History is get-one-run over a volatile registry — weaker than the claim on that one point. |
| C9 | No kill switch exists | **confirmed** | Zero pause/halt mechanisms repo-wide; detached run threads; choke points for one identified (P16.5). |
| C10 | No draft-vs-committed graph state | **confirmed** | All writes land directly; the only review-shaped machinery (skill review, scope approve) doesn't stage business data. |
| C11 | No cadence/scheduling for loops | **partial** | True for loops (manual `POST …/run` only, no cadence fields anywhere); but tested interval/event trigger primitives already exist in the mesh, unwired — the substrate for P16.5. |
| C12 | No maker/checker verification in loop runs | **confirmed** | No verifier step kind, no awaiting-approval state, no pause/resume; the executor is fire-and-forget. |
| C13 | No per-loop budget attribution/caps | **confirmed** | Only wall-clock constants (30s step / 300s run); ledger has no loop dimension; zero loop↔tokens linkage. |
| C14 | SKILL.md frontmatter fields | **confirmed** | Exactly `name`, `version`, `guest`, `clearance`, optional `description`. No cadence/autonomy/budget/gate/escalation keys — the envelope keys are additive (P16.4). |

**Additional facts the draft didn't claim but the PRD relies on** (verified):
`kanbrick-api` is axum on multi-threaded tokio with existing background-work
precedents (internal listener via `tokio::spawn`, a named reconcile-loop thread);
the Cockpit is strictly a polling HTTP client through Tauri commands (no
SSE/WebSocket assumptions needed); a service-identity primitive exists
(`ApiKeyService`, clearance-bound service keys) as precedent for non-interactive
principals; loops currently have a flat L1 creation floor and no
update/delete/versioning (acceptable — definitions are inert without runs;
revisit only if loop-sharing lands).
