# Handoff — Phase 10: Messenger + Visualizer (epic #81)

You are continuing the **Kanbrick-V1** build: an all-Rust "Firm Operating System"
with an L5 "Cockpit" agentic desktop (Tauri v2) layered on top. Work proceeds as
**independently-mergeable vertical slices**, one PR each. Phases 7–9 are done;
**you are starting Phase 10.**

## 0. First steps (do these before writing code)
1. `git fetch origin main && git checkout claude/sharp-newton-rljwv6 && git merge --ff-only origin/main`
   — develop ONLY on branch `claude/sharp-newton-rljwv6`; never push elsewhere without explicit permission.
2. Read **`docs/handoffs/cockpit-program.md`** (the program tracker — phases, reuse
   anchors, the "what unblocks which slice" staging table).
3. Read **epic #81** via the GitHub MCP (`mcp__github__issue_read`, owner `Tnsr-Q`,
   repo `kanbrick-v1`, issue 81) — it enumerates the P10 slices (P10.1–P10.7) with
   per-slice acceptance criteria and "Intended labels". **This is the authoritative
   slice list.** File each slice as a discrete issue if not already filed, then build
   one at a time. (The GitHub MCP server flaps in/out — if it's down, retry later or
   reconstruct scope from the handoff + ADRs; failure webhooks still arrive without it.)
4. Read ADRs **0011** (frontend: React+Vite, swappable above IPC), **0016** (Cockpit
   IPC auth contract — host-authoritative identity), **0002** (identity from the
   validated token, never the webview), **0008/0015** (CP/executor + tenancy).

## 1. Mission — P10: Messenger + Visualizer (req 2.1, 2.2)
The **messenger/brainstorm + live whiteboard** (req 2.2) and **sidecar/plugin
registration** (req 2.1). It is built on the existing **`EventBus`** — do NOT build a
new pub/sub fabric.

## 2. THE load-bearing reuse anchor — `EventBus` (`kanbrick-mesh/src/event.rs`)
A cloneable, thread-safe pub/sub fabric with a **replayable in-memory log**. Exact API:
- `EventBus::new()` / `Default` / `Clone` (internally `Arc<Mutex<Inner>>`).
- `emit(event: Event) -> usize` — append to the log + notify matching subs; an event
  with **no subscribers is logged, not dropped** (retained for replay). Handlers run
  *outside* the lock, so a handler may emit further events without deadlock.
- `subscribe(kind: impl Into<String>, handler: impl Fn(&Event) + Send + Sync + 'static) -> SubscriptionId`
- `subscribe_typed::<T: DeserializeOwned>(kind, handler: impl Fn(T) + ...)` — deserializes
  the event's JSON payload into a concrete schema; a mismatch is logged + skipped (the
  bad event still stays in the log).
- `unsubscribe(id)`, `history() -> Vec<Event>`, `replay(kind: Option<&str>, handler: impl Fn(&Event))`.
- `Event` is `kanbrick_core::abi::Event { kind: String, payload: serde_json::Value, .. }`.
The bus is already `.manage`d in the API (`AppState.bus`) and the mesh shares it.
**Messenger = typed events over this bus; whiteboard = same; visualizer = subscribe/replay + render.**

Other anchors (see the handoff's "Reuse anchors" table for exact paths):
- **`Scheduler`** (`kanbrick-mesh/src/scheduler.rs`) — `schedule_with_retry`/`schedule_interval`/`on_event`/`RetryPolicy` (for any loop/timed messenger behavior; mainly P11).
- **`ScopeGrants`** (`kanbrick-discovery/src/grants.rs`) — request→approve/deny→authorize→revoke/`expire_due`, fully audited (permission design).
- **Sidecar/plugin registration (req 2.1):** `kanbrick-api/src/caps.rs` `InvocationCaps`,
  `internal.rs` internal router + `x-kanbrick-internal-token` (fail-closed), `executor.rs`.
- **`AuditLog`** (`kanbrick-auth/src/audit.rs`) `record(&ctx, query)`; **`require_clearance`** (`kanbrick-auth`).

## 3. Staging (from the handoff) — what's unblocked
ADR-0011 (frontend) and P7 (cockpit shell) are landed, so **all P10 slices are
unblocked**. Backend slices (messenger/visualizer engine over EventBus, the API/IPC
surface) have no extra gate; their **UI siblings** use the React+Vite webview (ADR-0011).
Build backend-first, then its UI, mirroring P9.

## 4. The proven playbook (this exact loop got P7–P9 to green)
For each slice: **sync branch → read the issue + reuse anchors → design → implement
inline → adversarial review → fmt-check → commit → push → draft PR → babysit CI → merge.**

- **Implement cohesively, not in parallel.** Interdependent Rust↔TS↔React must match
  exactly (you can't compile to catch mismatches) — author them together.
- **Adversarial-review-as-compile-gate (critical — you cannot compile here):** after
  implementing, spawn **parallel `general-purpose` review Agents**, one per lens:
  (1) Rust/Tauri + `clippy -D warnings`, (2) TypeScript `tsc --strict` + React, (3)
  security/ADR-0016 (no secret/identity crosses IPC outward), (4) CI build-graph +
  manifest/lock + YAML, (5) AC-completeness vs the issue. Give each the file list +
  the contract files to read. Fold in must-fixes; **adjudicate false positives**
  (verify empirically — e.g. re-run `rustfmt --check` before "fixing" a claimed fmt
  issue). The **Workflow tool is unavailable this environment** (permission stream
  closed) — use plain parallel `Agent` calls in one message.
- **Then commit + push + open a draft PR**, and babysit: subscribe / watch the PR;
  CI *failures* arrive by webhook (diagnose via `mcp__github__get_job_logs`, fix,
  amend, force-push); CI *success/merge* are NOT delivered, so arm an **hourly cron**
  (`CronCreate`, off-minute) to re-poll and report green / self-delete on merge.

## 5. Environment & operational constraints (hard-won — read carefully)
- **You cannot compile** workspace crates or the cockpit locally: the pinned
  toolchain (`rustc 1.94.1`) and crates.io **tarball downloads are network-blocked**
  (index resolves, sources 403); the Tauri toolchain is absent. **CI is the only
  compile/clippy/test gate.**
- **`rustfmt` works offline** (it's a parser): always run
  `RUSTUP_TOOLCHAIN=stable rustfmt --edition 2021 --check <files>` before committing.
  (The default toolchain 1.94.1 isn't installed — the `RUSTUP_TOOLCHAIN=stable`
  override is mandatory for any rustup-proxied tool.)
- **CI gates:**
  - `ci.yml` (workspace): `cargo fmt --all --check`, `cargo clippy --workspace
    --all-targets --all-features -- -D warnings`, `cargo build/test --workspace
    --all-features`, `RUSTFLAGS=-D warnings`, rustc 1.94.1 / edition 2021, **no
    path filter** (runs on every PR to main). No `--locked`.
  - `cockpit.yml` (path-filtered to `cockpit/**` + the crates it bundles, incl.
    `kanbrick-providers/**`): `tsc && vite build`, `cargo fmt --check`, `cargo clippy
    --all-targets -- -D warnings`, `cargo test`, `tauri build`, login→/me smoke. The
    cockpit is its **own workspace** (`cockpit/src-tauri`, excluded from root); its
    `Cargo.lock` is gitignored (regenerated). It depends on workspace crates by path
    (`../../kanbrick-providers`) — a valid cross-workspace pattern.
  - A **"Validate Kubernetes manifests"** check runs on `deploy/k8s/**` changes.
- **Pushing to `claude/sharp-newton-rljwv6` does NOT trigger CI** (workflows run on
  push/PR to `main`). Useful: push → review → open the PR (which triggers CI) only
  once clean. Open PRs **as draft**.
- **Git/PR conventions:** `git push -u origin claude/sharp-newton-rljwv6` with
  exponential backoff retries. Commit messages END with:
  `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>` and
  `Claude-Session: <your session URL>`. PR bodies END with the
  `🤖 Generated with [Claude Code]` line + the session URL. **Never** put the model
  id string in any committed artifact (commits/PRs/code) — chat only. GitHub only via
  the `mcp__github__*` tools, scoped to `tnsr-q/kanbrick-v1`; be frugal with PR comments.
- After each merge, **update `docs/handoffs/cockpit-program.md`** (the P10 row + a
  progress note) in the same PR.

## 6. Clippy / build gotcha checklist (every one of these bit a P9 slice)
- `clippy::type_complexity` fires on nested `Mutex<HashMap<K, V>>` in a field or a
  `lock()` return — **alias it**: `type X = HashMap<..>;`. (default-deny group.)
- Calling a trait method on a **`dyn Trait`** object does NOT require the trait in
  scope → `use` of it is flagged **unused**; on a **concrete** type it IS required.
- Unused crate dependency (e.g. an unneeded `serde_json`) won't fail CI but clean it
  (and drop it from `Cargo.lock` if you do).
- `std::thread::JoinHandle` is **not** `#[must_use]` (tokio's is) — dropping it is fine.
- Tauri v2: use **single-word command param names** to sidestep camelCase↔snake_case
  conversion; app commands + the Channel API work under the existing **`core:default`**
  capability (no `capabilities/*.json` change needed).
- Hand-editing `Cargo.lock`: add new `[[package]]` blocks in **alphabetical** order;
  disambiguate `thiserror` as `"thiserror 2.0.18"` (two versions in the lock). CI runs
  without `--locked`, so it self-heals, but keep it consistent.
- New workspace crate: add to root `Cargo.toml` `members`; only add to
  `[workspace.dependencies]` if another member depends on it.

## 7. Architecture patterns to mirror (from P9)
- **Trait-seam + injected backend**: define a trait + an in-memory/CI-testable impl
  in the workspace; defer the heavy/network/native backend (and document it). This is
  how every "can't compile it here" piece stayed CI-green (transport seam, custody
  store, egress gate, audit sink).
- **Host-authoritative IPC** (ADR-0016): identity/secrets are resolved host-side from
  the session JWT; the webview supplies neither. For live host→webview streaming use a
  **`tauri::ipc::Channel<T>`** (see `cockpit/src-tauri/src/providers.rs` P9.4 — the
  template for a live messenger/whiteboard feed). Typed IPC lives in
  `cockpit/src/api.ts`; React surfaces are `view`-switched in `cockpit/src/App.tsx`.
- **Serde-tagged enums** (`#[serde(tag = "...", rename_all = "snake_case")]`) mirror
  to TS discriminated unions 1:1.
- Workspace crate layout for a new backend piece: `kanbrick-<name>/{Cargo.toml,src/lib.rs}`,
  deps via `*.workspace = true`, pure + unit-tested.

## 8. Definition of done (per slice)
Vertical slice merged to `main` with: green `ci.yml` (+ `cockpit.yml` if it touched
the cockpit); unit/integration tests for the new behavior; `rustfmt`/`tsc` clean;
adversarial review cleared; the handoff doc updated; the issue closed by the PR.

Phase 9 (BYO-AI) just completed: `kanbrick-providers` (trait+Usage+adapters+custody),
`kanbrick-tokens` (priced ledger), `kanbrick-egress` (DLP+allowlist+RBAC gate), and the
cockpit BYO-AI streaming console. Build P10 the same way. Good luck.
