# HANDOFF — Kanbrick-V1 Phase 11 deepening: P11.5 (MCP tool steps) → P11.6 (authoring UI)

Continuing the L5 "Cockpit" program (Phases 7–15) on the finished L1–L4 all-Rust "Firm OS".
Read `docs/handoffs/cockpit-program.md` (program tracker — keep it updated), `docs/ARCHITECTURE.md`,
`docs/adr/`. Phase epic: **#82**. **Build these two slices ONE AT A TIME** (P11.5 first, merge, then P11.6).

## Mission
Finish the **deepening half of Phase 11 — Skill/Loop Ecosystem (Req 2.3/2.5)** as independently-mergeable
vertical slices. The **walking skeleton is complete and merged**: author a SKILL.md → publish to a versioned
registry → bind onto a grant-gated scope → compose a `(:Loop)` of steps → run through the real
`authorize_skill` gate on the mesh `Scheduler` → watch each step live in the Cockpit. The **loop step is the
polymorphic unit**: **guest** (P11.3) and **provider/LLM** (P11.4) steps both run; P11.5 adds the **third
kind** (external MCP tool), and P11.6 makes the **create-side usable in the app**.

## Status — what's merged (do NOT rebuild)
- **P11.1 (#127)** — skill schema/registry: `kanbrick-loops` (`SKILL.md` + `parse_skill_md`) +
  `kanbrick-store::skill_registry` (`(:Skill)`/`(:SkillVersion)`, ADR-0012).
- **P11.2 (#128)** — grant-lifecycle HTTP surface (`/me/scope-requests`, `/me/scopes`) over `ScopeGrants`.
- **P11.2b (#129)** — skill-registry ⇄ grant bridge: `POST/GET /me/skills`, `GET /me/skills/{name}`,
  `POST/GET /me/scopes/{id}/skills` (bind a published edition onto a scope). `kanbrick-api/src/skills.rs`.
- **P11.3 (#130)** — loop run engine (ADR-0013): `(:Loop)`/`(:LoopStep)` (`kanbrick-store/src/loop_registry.rs`)
  compiled onto `kanbrick-mesh::Scheduler`, gated per-step by `authorize_skill`; routes `POST/GET /me/loops`,
  `GET /me/loops/{id}`, `POST /me/loops/{id}/run`, `GET /me/loops/runs/{id}`. `Scheduler` is in `AppState`.
  Run history is **in-process** (`kanbrick-api/src/loops.rs` `LoopRunRegistry`).
- **P11.7 (#131)** — thin run-and-watch Cockpit UI (`cockpit/src/LoopRunner.tsx` + `cockpit/src-tauri/src/loops.rs`):
  pick a loop, run, watch each step's `TaskStatus` live over a Tauri Channel (`watch_run` self-stops on terminal).
- **P11.4 (#132)** — loop **provider steps** (ADR-0019): a `(:LoopStep)` is polymorphic (guest XOR provider).
  A provider step runs an LLM completion via an injected `ProviderFactory` seam
  (`kanbrick-api/src/provider_runtime.rs`, echo default); `provider_ref` selects **model only**; the host
  resolves the caller's key from `AppState.provider_keys` **by `caller.user_id`** (never from the step) and
  injects it (the security property). Step schema gained opaque `provider`/`model` strings.

`main` tip: merge of #132. Develop on the rolling program branch (see GIT / MERGE). Author ADRs **inside** the
implementing slice. Current ADR high-water mark: **0019** → next is **ADR-0020**.

---

## SLICE 1 — P11.5: external MCP tool-call steps ← "the third step kind"

**Goal.** A loop step can be an **MCP tool-call** (a third kind alongside guest and provider). It calls an
external tool via a **managed `tachyon-mcp` sidecar** under **host-authoritative identity**, gated by the
**same** `authorize_skill`, with the **core staying no-egress** (ADR-0017) and **no second WASM runtime**
(ADR-0014). **Author ADR-0020.**

**The locked decision (probe P8.3, `docs/probes/p8.3-mcp-bridge.md`):** wrap `tachyon-mcp` as a **managed
sidecar** (reuse the P7.2 `SidecarSupervisor` spawn→`/health`-gate→supervise→kill pattern,
`cockpit/src-tauri/src/sidecar.rs`), **NOT** a `HostServices` backend (that trait is the guest↔host *graph*
ABI, a different concern). **Identity passthrough:** mint a per-invocation capability
(`InvocationCaps::mint`, `kanbrick-api/src/caps.rs`) bound to the caller's `FirmContext`; the MCP sidecar
receives **only** the opaque cap + the specific tool + the args the `ProjectScope` authorizes — never the
identity bytes; results return and the host **re-authorizes** before applying. The `x-kanbrick-internal-token`
(fail-closed) gates the host↔sidecar control channel (same gate as the internal RPC surface,
`kanbrick-api/src/internal.rs`).

**Build it like P11.4 (the closest template — copy its shape):**
1. **Schema** (`kanbrick-store/src/loop_registry.rs`): add opaque `tool` (and optionally `tool_args` JSON
   string) to `LoopStepRecord`/`LoopStepSpec`, exactly as P11.4 added `provider`/`model` — plain strings,
   always SET (empty = not an MCP step), in `STEP_PROJECTION` and the SET, with a store unit test. Keep
   `kanbrick-store` free of any MCP dep. A step's kind: `tool` non-empty → mcp-tool; else `provider` non-empty
   → provider; else guest.
2. **Seam** (`kanbrick-api`, new module e.g. `tool_runtime.rs`): an **injected `McpBridge`** (or `ToolRunner`)
   trait on `AppState` (mirror `ProviderFactory` exactly): `fn call_tool(&self, cap: &str, tool: &str, args:
   &JsonValue) -> Result<JsonValue, String>`. Default = a **no-network stub** (echo/canned, like
   `EchoProviderFactory`) so the slice is CI-testable; the **real** impl (managed `tachyon-mcp` via
   `SidecarSupervisor` + `InvocationCaps` + `x-kanbrick-internal-token`) is injected at deploy via
   `AppState::with_mcp_bridge(...)`. **No live subprocess/socket in core/CI** — same discipline as P9.4/P9.6/P11.4.
3. **Executor** (`kanbrick-api/src/loops.rs` `execute_loop`): add a **third branch** after the `authorize_skill`
   gate. For an mcp-tool step: mint `state.caps.mint(caller, TTL)`, call `mcp_bridge.call_tool(&cap, &step.tool,
   &args)` where `args` derives from the piped payload (+ `tool_args`), **revoke the cap** after (like
   `invoke_guest` in `lib.rs`), pipe the result onward. The tool/args come from the step + payload, **never
   identity** (the cap carries identity opaquely). Same `authorize_skill` gate as guest/provider steps (the
   skill supplies scope + clearance floor). `AppState` already holds `caps: Arc<InvocationCaps>`.
4. **Route**: `POST /me/loops` gains an optional per-step `tool_ref { tool, args? }` (validate non-empty tool →
   else 400, like `provider_ref`); `GET` surfaces it. A step may be guest XOR provider XOR mcp-tool — reject a
   step that sets more than one (400).
5. **Tests** (`kanbrick-api/tests/loops.rs`): an mcp-tool step runs to completion via an **injected recording
   `McpBridge`** that asserts it received the **opaque cap** (resolvable to the caller via `caps.resolve`) + the
   tool + args — proving identity stays host-side and the step names only the tool; a step naming an unknown/empty
   tool → fails; create validation (empty tool, or multiple kinds set) → 400. Guest/provider steps unaffected.
6. **ADR-0020** (managed-sidecar MCP tool steps; capability passthrough; injected seam; no-egress core / no
   second runtime). Update the tracker §5 P11 row + §6 ADR index + the `loops.rs`/`loop_registry.rs` module docs.

**Why a seam, not the live sidecar, in this slice:** the `tachyon-mesh` submodule is proxy-blocked (HTTP 403,
P8.1/P8.3) and there's no local Rust compile — so ship the seam + stub + the security/identity property (the
deliverable), with the real managed sidecar injected at deploy. Mirrors how every BYO-AI/egress slice shipped.

**Optional small companion (epic's "P11.5" — durable run history, deferred from P11.3):** if time allows, a
clean ~1-file slice — persist each run's per-step status from the in-process `LoopRunRegistry` to SparrowDB
(`(:LoopRun)`/`(:LoopRunStep)`, ADR-0001 dialect, like `loop_registry`) so `GET /me/loops/runs/{id}` survives a
restart. Keep it separate from the MCP work; it does not block P11.6.

---

## SLICE 2 — P11.6: skill-authoring + library + loop-builder UI ← "the create-side, usable in the app"

**Goal.** A Cockpit React surface to **author/publish a SKILL.md**, **browse the scope-filtered skill library**,
**bind** a skill onto a scope, and **build a loop** of ordered steps (guest · provider · mcp-tool, with
provider/tool labelling) — so the create-side of the ecosystem is usable in the app, not just over the API.
**[AFK]**, pure frontend; **no new ADR** (reuses ADR-0011 frontend + ADR-0016 IPC). Blocked-by P11.2 + P8.5 — both done.

**Reuse the P11.7 UI pattern verbatim** (`cockpit/src/LoopRunner.tsx`, `cockpit/src/api.ts`,
`cockpit/src-tauri/src/loops.rs`, `cockpit/src-tauri/src/auth.rs` `authed_get`/`authed_post`,
`cockpit/src/App.tsx` view wiring, `cockpit/src/App.css`). The cockpit is a **pure HTTP client** of the bundled
`kanbrick-api` sidecar (host-held Bearer injected host-side, ADR-0016; the webview supplies no identity).

**Build (one cohesive panel or a few):**
1. **Tauri commands** (`cockpit/src-tauri/src/skills.rs`, new — mirror `loops.rs`): `publish_skill(skill_md)` →
   `POST /me/skills`; `list_skills()` → `GET /me/skills`; `skill_history(name)` → `GET /me/skills/{name}`;
   `bind_skill(scope_id, skill_name, version?)` → `POST /me/scopes/{id}/skills`; `list_scopes(project)` →
   `GET /me/scopes?project=…` (to pick a scope to bind onto / reference in a loop step); `create_loop(name,
   steps)` → `POST /me/loops` (steps carry guest | provider_ref | tool_ref). All through `authed_get`/`authed_post`.
   Register in `cockpit/src-tauri/src/lib.rs` `generate_handler!`. DTOs mirror the API 1:1 (serde snake_case);
   `#[derive(...)]` like the existing modules. Tauri v2 arg convention: JS camelCase ↔ Rust snake_case (the default).
2. **`api.ts` bindings** (mirror the P11.7 section): types + `invoke` wrappers for the above. The SKILL.md
   composer can build the frontmatter text client-side from a form (`---\nname: …\nversion: …\nguest: …\nclearance:
   …\ndescription: …\n---\n\n<body>`) — the inverse of `SkillManifest::to_skill_md` (see `kanbrick-loops`).
3. **React** (`cockpit/src/SkillStudio.tsx` or similar, + a footer button in `App.tsx` gated behind `auth==="in"`):
   - **Author**: a frontmatter form (name/version/guest/clearance/description) + a markdown body `<textarea>` →
     compose SKILL.md → publish; show the returned `SkillVersionRecord` (incl. `source` host-stamped, `seq`).
     Malformed → the API's `400 invalid_skill_md` (surface the message).
   - **Library**: list `GET /me/skills` (latest per skill) with clearance badge + guest; click → version history.
   - **Bind**: pick a skill + one of the caller's active scopes (`GET /me/scopes?project=…`) → bind.
   - **Loop builder**: ordered step rows; each row picks a bound skill + scope and a **kind** — guest, or
     provider (`provider_ref {provider, model}`), or mcp-tool (`tool_ref {tool}`, once P11.5 lands) → `create_loop`.
     Then it shows up in the P11.7 `LoopRunner` to run/watch. Reuse the existing card/badge/chip/`btn-secondary`
     CSS vocabulary; add a focused CSS block like P11.7 did.
4. **Gate locally** (REAL gate for the TS): `cd cockpit && npx tsc --noEmit && npx vite build` (run
   `npm install` first if `node_modules` is absent). Cockpit-Rust: `cd cockpit/src-tauri && cargo +stable fmt`;
   clippy/test/`tauri build` are gated by the **cockpit CI** (`.github/workflows/cockpit.yml`). Adversarial-review
   the cockpit Rust (can't compile locally) + a UI lens.
5. Update the tracker.

**Note:** if P11.6 is built before P11.5, just omit the mcp-tool kind from the loop builder (add it when P11.5
lands). Provider-step labelling works as soon as P11.4 is in (it is).

---

## STILL REMAINING AFTER THESE (for the operator, not these two slices)
- **P11.8 [HITL]** — skill-publish **trust gate**: a dual-gate lead review (reuse `ScopeGrants::eligible_grantor`)
  before an authored skill is invocable by others. This also closes the **P11.2b cross-author re-publish gap**
  (a `SECURITY` note in `kanbrick-api/src/skills.rs` `publish_skill` marks it: any L3+ caller can re-publish an
  existing skill name and overwrite another author's `source`/`guest`/`min_clearance`). HITL — surface a design
  to the operator first, don't pre-empt.
- **Token-ledger wiring** is **Phase 12** (priced `kanbrick-tokens` ledger on provider-step `Usage` + budgets).

---

## HARD CONSTRAINTS / GOTCHAS (unchanged — these bit us repeatedly)
- **No local Rust compile.** `sparrowdb` submodule is NOT checked out AND crates.io is firewalled (403), so
  `cargo build/check/test` fails at workspace dependency resolution. **Compile gate = adversarial review + CI.**
  (`cargo +stable fmt -p <crate>` DOES work — no dep resolution. And `cd cockpit/src-tauri && cargo +stable fmt`.)
- **Frontend builds locally**: `cd cockpit && npx tsc --noEmit && npx vite build` — use as a real gate for UI.
- **CI catches what review misses.** A P11.3 review pass missed `clippy::too_many_arguments` (8/7 under
  `-D warnings`); CI caught it. The workspace CI is `cargo fmt --check` + `cargo clippy --workspace --all-targets
  --all-features -- -D warnings` + build/test. The cockpit CI also runs clippy `-D warnings` + `cargo test` +
  `tauri build` on `cockpit/src-tauri`. **Count function args (≤7 or `#[allow(clippy::too_many_arguments)]` with a
  justifying comment, as the `Scheduler` trigger fns + `execute_loop` do); watch dead_code (every enum variant
  constructed, every struct field read — serde counts) and unused imports.**
- **SparrowDB dialect (ADR-0001/0006)**: parameterized standalone `CREATE` unsupported → `MERGE (n {key})` then
  `MATCH … SET`. Reads use **bare-node projection** `RETURN n.prop` (NO `AS` aliases → nulls). Relationship
  `MERGE` must use the **non-parameterized** `store.execute(&format!(...))` path with inline single-quote-escaped
  values (ADR-0006/SPA-233). Always SET every property (empty string, never absent) so reads never null. See
  `kanbrick-store/src/loop_registry.rs` + `skill_registry.rs` (copy them).
- **Host-authoritative identity (ADR-0002/0016)**: the actor is ALWAYS `AuthedContext`'s `FirmContext`
  (`ctx.email`/`user_id`/`clearance`), NEVER a body field. The loop `owner`/run `caller`, the resolved provider
  key (by `caller.user_id`), and the MCP capability (bound to the caller's `FirmContext`) all come from the host.
  A loop step names a *tool/model* only — never a credential or identity.
- **Flaky tools**: `AskUserQuestion` and `Workflow` intermittently fail "Tool permission stream closed". Fallback
  that WORKS: parallel `Agent` calls (general-purpose / Explore) for review/mapping; fall back to a plain-text
  question if AskUserQuestion errors; decide yourself with sound defaults when you can't ask.
- **GitHub MCP flaps** (disconnects for stretches). When down: `git push` still works; the github tools reappear
  (`ToolSearch "select:mcp__github__create_pull_request"`). Open the draft PR + subscribe when reconnected.

## THE PLAYBOOK (per slice — one at a time, merge before the next)
sync (`git fetch origin main && git reset --hard origin/main`) → read epic #82 + the reuse anchors → design (for
P11.5, research the MCP/sidecar substrate first; for an [HITL] item, get operator sign-off) → implement →
**adversarial-review-as-compile-gate** (≈5 parallel `Agent` lenses: compile/types, clippy `-D warnings`,
store-dialect [for store code], security/ADR, AC/tests-&-regression; for cockpit Rust add a UI/contract lens; fix
confirmed blockers) → `cargo +stable fmt -p <crate>` (+ `tsc`/`vite build` and `cockpit/src-tauri` fmt for UI) →
commit → push → **draft PR** (base `main`) → subscribe + babysit CI (fix failures; re-kick) → **the operator
merges** (NEVER self-merge) → post-merge cleanup (`git fetch origin main && git reset --hard origin/main`; delete
the babysit cron) → update `docs/handoffs/cockpit-program.md`. Then the next slice.

## SUBSTRATE ALREADY BUILT (reuse, don't rebuild)
- **Loop run engine** `kanbrick-api/src/loops.rs` (`execute_loop` — the guest/provider branch; add the mcp-tool
  branch here) + `kanbrick-store/src/loop_registry.rs` (the polymorphic step schema; add `tool`).
- **Provider-step seam (the P11.5 template)** `kanbrick-api/src/provider_runtime.rs` (`ProviderFactory` injected
  trait + echo default + `with_provider_factory`) — copy this exact shape for the `McpBridge` seam.
- **Capability passthrough** `kanbrick-api/src/caps.rs` (`InvocationCaps::mint/resolve/revoke`, already in
  `AppState.caps`) — the identity-passthrough mechanism for MCP (mint→hand opaque cap→resolve host-side→revoke).
- **Sidecar supervision** `cockpit/src-tauri/src/sidecar.rs` (`SidecarSupervisor`: spawn→`/health`-gate→supervise
  →kill) — the managed-`tachyon-mcp` pattern (deploy-time `McpBridge` impl). Internal RPC gate
  `kanbrick-api/src/internal.rs` (`x-kanbrick-internal-token`, fail-closed).
- **Permission spine** `kanbrick-discovery::ScopeGrants` (`authorize_skill` → `(Skill, ProjectScope)`; the run
  gate every step kind shares). Base scope via `ClearanceScope::resolve(store, &ctx)` (`kanbrick-auth`).
- **Skill registry + bridge** `kanbrick-store::{publish_skill_version,list_skills,list_skill_versions,
  latest_skill_version,SkillVersionRecord}` + `kanbrick_loops::{parse_skill_md, SkillManifest::to_skill_md}` +
  the `/me/skills` + `/me/scopes/{id}/skills` routes (`kanbrick-api/src/skills.rs`) — the API the P11.6 UI drives.
- **Cockpit UI pattern** `cockpit/src/LoopRunner.tsx` + `cockpit/src/api.ts` (Channel poller, `authed_*`,
  DTO-mirror types) — the template for the P11.6 panels. `cockpit/src/App.tsx` view wiring, `App.css` vocabulary.
- Full reuse-anchor table + ADR index: `docs/handoffs/cockpit-program.md` §4, §6.

## ROUTE/CODE CONVENTIONS (mirror these)
- Handler: `async fn h(State(state), AuthedContext(ctx), [Path/Query], [Json(body)]) -> Result<Json<T>, ApiError>`
  — **Json LAST** (axum: one body extractor, last). First line: `require_clearance(&ctx, CONST)?`. `mod x;` in
  `lib.rs` (alphabetized); register routes in `router()` before `.with_state(state)`. Per-user surfaces under
  `/me/...`. Templates: `kanbrick-api/src/{skills,loops,provider_keys,grants}.rs`.
- Injected seam (the P11.4/P11.5 pattern): a `pub trait X: Send + Sync` in its own module; `pub use` it from
  `lib.rs`; `AppState` holds `Arc<dyn X>` with a no-network default + a `with_x(...)` builder; the real impl is
  injected at deploy. Integration tests inject a recording impl to prove the host-side property.
- DTOs: domain types that aren't serde get thin DTOs; `#[serde(skip_serializing_if="Option::is_none")]` for
  optional fields. The grant `Skill`/`ProjectScope` are NOT serde (use DTOs); `SkillVersionRecord`/`LoopRecord`
  ARE (return directly). New ADRs authored INSIDE the slice (`# ADR NNNN — Title` / Status/Date/Context/Deciders /
  ## Context / ## Decision / ## Consequences).

## GIT / MERGE
- Branch **`claude/phase-11-skill-loop-z9sg2z`** (the rolling program branch — the harness sets it per session;
  use whatever branch the new session is started on; do NOT invent a new one). PRs base `main`, **draft**. Push
  `git push -u origin <branch>` with exponential-backoff retry. Commit trailer `Co-Authored-By: Claude
  <noreply@anthropic.com>` + the session line. Plain `Part of #82` (no backticks). **Never** put a model-id string
  in commits/PRs/code.
- **The operator drives every merge** (build → draft PR → babysit → they mark ready + merge). After merge:
  `git fetch origin main && git reset --hard origin/main`; delete the babysit cron. Stop-hook "Unverified commit"
  warnings about GitHub's own merge commits (committer `noreply@github.com`) are a benign FALSE POSITIVE — never
  amend (shared merged history). When GitHub MCP is down, arm a recurring `CronCreate` that probes the github
  tools and opens the draft PR + subscribes when it reconnects. Webhooks deliver CI *failures* + review comments
  but NOT CI success / merges — arm an hourly babysit `CronCreate` to catch the merge + re-check, re-arming
  silently. GitHub access scoped to `tnsr-q/kanbrick-v1` only. Be frugal with PR comments.

First moves: (1) sync to `main`; (2) read epic #82 + `docs/probes/p8.3-mcp-bridge.md` + `kanbrick-api/src/{loops,
provider_runtime,caps}.rs`; (3) build **P11.5** (designed above), merge it; (4) then **P11.6**.
