# Phase 3 Handoff — Orchestration: WASM Runtime (Tachyon-Mesh)

> **Read this together with the frozen PRD** (the operator will give it to you).
> The PRD tells you *what* Phase 3 must deliver (deliverables 3.1–3.6, checkpoints).
> This document tells you *the repo-specific reality the PRD does not know* —
> the state of the code, the vendored-upstream truth, the SparrowDB landmines,
> and the exact way Phases 0–2 were built so Phase 3 matches.

**Scope:** issues **#21–#29**. Branch: develop on **`claude/agents-codebase-review-km2nkg`**
(or whatever branch the operator specifies). Do **not** open a PR unless asked.

---

## 1. Where the project stands

Phases 0, 1, and 2 are **done, green, and pushed**. Build is one workspace.

| Layer | Crate | State |
| --- | --- | --- |
| Core | `kanbrick-core` | `FirmContext`, `ClearanceLevel` (L1<…<L5), `Status`, id newtypes, `Error`/`ErrorKind`, graph label vocab. **ABI types for #22 belong here.** |
| L3 Brain | `kanbrick-store` | Embedded SparrowDB wrapper: `Store` (open/close/checkpoint), `Params`, typed `query::<T>`/`scalar_i64`, `schema`, `Migrator`, `seed`. |
| L1 Guard | `kanbrick-auth` | JWT (`JwtAuthenticator`), Argon2id, `LoginService`, `require_clearance`, `ClearanceScope`, **`GuardedStore`** (audited + clearance-filtered reads), `AuditLog`, `ApiKeyService`. |
| API | `kanbrick-api` | Axum 0.8: `POST /login`, `GET /me`, clearance-gated `GET /admin`; structured 401/403. lib + bin. |
| L2 Nerves | **`kanbrick-mesh`** | **Phase 3 target.** Currently a trivial scaffold. |
| L4 Map | `kanbrick-discovery` | Phase 4 scaffold. |
| Guests | `guests/{valuation,reporting,compliance}` | Trivial lib stubs; become real WASM guests in Phase 5. Your **echo guest (#21)** is the first real one. |

**Build / test / lint (all must stay green):**
```bash
git submodule update --init --depth 1 crates/sparrowdb crates/tachyon-mesh
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```
Toolchain is pinned in `rust-toolchain.toml` (currently **1.94.1** — see §6).
`wasm32-wasip1` is already an installed target. `wasmtime = "45"` is already pinned
in the workspace `[workspace.dependencies]` but **nothing uses it yet** — Phase 3 is
where it gets wired in.

---

## 2. How this project is built (match this exactly)

These conventions are why Phases 1–2 went cleanly. Follow them.

1. **De-risk the vendored upstream FIRST, before writing any wrapper code.**
   For each phase the hard part is the vendored crate. *Before* designing,
   confirm: is it a usable library or a binary? does it compile under our
   toolchain? what is its *real* API (read the source, not the README)? Write a
   throwaway probe test (`kanbrick-*/tests/zz_probe.rs`, delete after) that calls
   the real API on real data. This single habit caught the SparrowDB dialect gaps
   and the Ironclaw "it's a binary" surprise early instead of mid-build.
2. **One vertical slice per issue, in dependency order.** Respect each issue's
   "Blocked by". Build → test → `clippy -D warnings` → `fmt` green at each step.
3. **Tests prove the issue's acceptance criteria.** Each GitHub issue has an
   explicit checklist; write tests that hit each bullet. Seed-backed integration
   tests read `seed/kanbrick_seed_data.cypher` via `Migrator::firm(...)`.
4. **Record non-obvious upstream behavior in an ADR.** `docs/adr/` — when you
   discover a quirk or make a one-way-door decision, write it down (see
   ADR-0001). HITL issues (#21, #22) especially warrant a short ADR.
5. **Flag deviations from the PRD loudly, then proceed.** The PRD makes
   assumptions that have repeatedly proven wrong about the upstreams (toolchain,
   library-vs-binary, Cypher dialect). When you must deviate, do it, and call it
   out in the commit + your summary. Don't silently diverge; don't block waiting
   for permission on a forced technical correction.
6. **Commit cadence:** small, reviewable commits per logical slice. Commit
   messages end with the session URL line that's already in git history. **Never
   push to a different branch; never open a PR unless explicitly asked.**

---

## 3. The Tachyon-Mesh reality (this is the crux of Phase 3)

I initialized and read `crates/tachyon-mesh`. **It is not a drop-in library you
call.** Findings:

- It is a **multi-crate workspace** (`core-host`, `tachyon-client`, `faas-sdk`,
  `tachyon-mcp`, `ebpf-probes`, `turboquant-sys`, …).
- `core-host` is the WASM host: **~50,000 lines across 76 files** — mesh control
  plane, leader election, **eBPF (`aya`)**, **AI inference (`candle`/ONNX)**,
  telemetry, UDS fast-path. The heavy bits are feature-gated, but it is a large,
  infra-coupled crate.
- Its host↔guest ABI is **WASM Component Model + WIT** (`wasmtime`
  `component-model`), and it targets **`wasm32-wasip2`** (we only install
  `wasip1`). There's already a `graph::workspace-graph` WIT resource on the host
  side — conceptually similar to our `query_graph`, but in their world.
- `faas-sdk` is just a **proc-macro** (`faas_handler`), not a rich binding lib.
- It pins **rustc 1.96.0** and **`wasmtime 45.0.0`** (wasmtime matches our pin;
  the toolchain does **not** — see §6).

### The decision you must make on day one: Path A vs Path B

This is the same fork we hit with Ironclaw in Phase 2 (Ironclaw turned out to be
a binary, so we built `kanbrick-auth` on its *primitives* — `jsonwebtoken` +
`argon2` — not on Ironclaw itself).

- **Path A — depend on Tachyon-Mesh `core-host`.** Maximum fidelity to the PRD's
  "Tachyon-Mesh runtime", but: 50k LOC, WIT/component-model complexity, a
  1.96.0 toolchain bump, `wasip2`, and heavy optional deps that may not build
  cleanly in this container. High risk, slow.
- **Path B — build `kanbrick-mesh` directly on `wasmtime` 45** (already pinned),
  defining *our own* minimal Host-Guest ABI. The PRD's own risk register says
  to *"Pin version; wrap ABI in stable kanbrick types"*, and issue #22 says the
  ABI traits **live in our core crate**. wasmtime is the exact primitive
  Tachyon-Mesh itself uses. This is fully in our control, testable, fast, and
  consistent with how Phase 2 handled Ironclaw.

**My strong recommendation: Path B.** Build the runtime on `wasmtime` 45 with
WASIp1 guests; keep Tachyon-Mesh `core-host` as a *reference* for ABI/WIT design
(and as the thing we can graduate to later). **But this is a genuine
architectural fork and a HITL-flavored call — confirm it with the operator
before committing a lot of code**, the same way the toolchain bump and the
"clearance-filter-in-Rust" calls were surfaced. Verify first:
```bash
# Does the heavy host even build here without AI/eBPF? (expect it to be slow/ugly)
cargo build -p core-host --no-default-features 2>&1 | tail
# Does plain wasmtime 45 compile in our workspace? (the Path B foundation)
# add a tmp dep or a scratch crate and `cargo build`.
```

> If you choose Path B, **rename the boundary honestly**: the crate is
> `kanbrick-mesh` and it provides a Tachyon-Mesh-*compatible* orchestration layer
> built on wasmtime. Note the deviation in the commit and an ADR. The PRD also
> names a separate **`kanbrick-wasm-host`** crate (crate inventory) that does
> **not** exist in our workspace — decide whether to add it or fold the host into
> `kanbrick-mesh`, and say which.

---

## 4. SparrowDB landmines that *will* bite Phase 3

`query_graph` (#24) runs Cypher against SparrowDB from inside a guest, so the
quirks from Phase 1 apply. **Read `docs/adr/0001-sparrowdb-cypher-capabilities.md`
in full.** The ones that matter here:

- **Variable-length traversal works** (`-[:R*1..n]->`) — *project nodes*, never
  `count()` over a path.
- **Don't alias bare-node projections to a different name** (`RETURN p.x AS y`
  returns `Null`). Project un-aliased; the store's row-mapper strips the `p.`
  prefix onto matching struct field names.
- **`WHERE` on a bare single-node scan is unreliable** (returns null rows).
- **Booleans round-trip as integers** — store/read flags as `i64` 1/0.
- **Parameterized writes use `MERGE` (inline props) or `MATCH … SET x = $v`** —
  parameterized `CREATE` is rejected.
- **Clearance filtering is done in Rust, not Cypher `WHERE`** (ADR-0001 §Decision).

---

## 5. The security thread (do not break it)

Phase 2 built the enforcement; Phase 3 must route *through* it, not around it.

- **`query_graph` MUST go through `kanbrick_auth::GuardedStore`, not the raw
  `Store`.** `GuardedStore::new(&store, &ctx)` resolves the caller's
  `ClearanceScope`; `query_persons`/`query_companies` audit (#19) and
  clearance-filter (#17) every read. Issue #24's acceptance ("an L3 guest sees
  filtered results", "recorded in the audit log under the guest's identity") is
  *exactly* `GuardedStore`. Wire the guest's host function to it.
- **`FirmContext` is host-authoritative (#23).** The host serializes the context
  into the guest; the guest reads it via `get_firm_context()` and can **never**
  set/forge/escalate it. A guest-supplied context must be rejected. A guest
  invoked with no context gets a clean host error.
- **Service identity for guests already exists:** `ApiKeyService` issues scoped,
  clearance-bounded keys and `validate()` returns a `FirmContext` with
  `email = "service:<name>"`, `roles = ["service"]`. Use it if a guest acts on
  its own behalf rather than a user's.
- `FirmContext` and `ClearanceLevel` are `serde` types — they already round-trip
  through JSON (that's #22's "survives serialize/deserialize across the
  boundary"). JSON is the path of least resistance for the boundary format
  unless you have a reason to choose otherwise (document the choice — #22).

---

## 6. Toolchain, submodules, CI

- **Toolchain conflict:** Tachyon-Mesh pins **1.96.0**; the workspace is on
  **1.94.1** (bumped from 1.85 in Phase 1 for SparrowDB). If you take Path A (or
  even compile `core-host` to study it), you'll likely need to bump
  `rust-toolchain.toml` to **1.96.0** — and then **re-verify SparrowDB, Ironclaw
  reference, graphify-rs, and the whole workspace still build**. Highest-wins, as
  documented in the toolchain file's comment. If you take Path B you may not need
  the bump at all (wasmtime 45 builds on 1.94.1 — verify).
- **Targets:** `wasm32-wasip1` is installed. **`wasm32-wasip2` is not** — add it
  (`rust-toolchain.toml` `targets`) only if you go component-model.
- **Submodules:** `crates/tachyon-mesh` is now initialized locally. **CI
  (`.github/workflows/ci.yml`) only treats `crates/sparrowdb` as build-required**
  today. If `kanbrick-mesh` ends up depending on a tachyon-mesh crate, add a
  required init step for it (copy the SparrowDB pattern). If Path B (wasmtime
  only), no CI submodule change is needed.
- **CI gates:** fmt check, `clippy -D warnings`, build, test — keep all green.

---

## 7. Issue-by-issue guidance (#21–#29)

Dependency order and HITL flags below. Acceptance criteria are paraphrased — read
the live issues for the authoritative checklist.

- **#21 — Runtime init & echo guest (HITL).** Foundation. Init the runtime + a
  WASM module registry; load an `echo` guest that returns input unchanged;
  dispatch and assert identical bytes; guest shows name+version in the registry.
  **HITL:** memory limits, fuel metering, WASI capabilities are human decisions —
  propose defaults, get sign-off, record in an ADR. *This is where the Path A/B
  choice lands.* The echo guest is your first real `wasm32-wasip1` build.
- **#22 — Host-Guest ABI (HITL, one-way door).** Define `HostFunctions`
  (`query_graph`, `get_firm_context`, `emit_event`, `log`) and `GuestModule`
  (`name`, `version`, `execute`, `health_check`). **ABI types live in
  `kanbrick-core`.** Choose + document the boundary serialization format
  (JSON is the low-risk default; `FirmContext` and a query result already
  serde-round-trip). Get the trait surface approved before building on it.
- **#23 — FirmContext passthrough (AFK, security).** Host serializes context in;
  guest reads via `get_firm_context()`; forged context rejected; L3 token ⇒ guest
  observes L3; missing context ⇒ clean host error. Blocked by #22, #16.
- **#24 — `query_graph` host function (AFK).** The WASM→SparrowDB round-trip,
  **through `GuardedStore`** (clearance + audit). Param-safe; an L3 guest sees
  filtered results; "count persons ⇒ 12" with an audit entry under the guest's
  identity. Blocked by #23, #9.
- **#25 — Scheduler: immediate + timeout (AFK).** Dispatch to a named guest;
  configurable timeout kills overruns with a structured error; per-guest
  concurrency limit queues excess; tasks have unique ids + queryable status.
  wasmtime **epoch interruption / fuel** is the timeout mechanism on Path B.
- **#26 — Scheduler: cron + event-triggered + retry (AFK).** Recurring + event
  triggers; exponential-backoff retry; cancellation. Blocked by #25.
- **#27 — Event bus (AFK).** `emit_event` + typed subscriptions
  (`ValuationComplete{…}` ⇒ reporting guest); typed schemas; persisted/replayable;
  no-subscriber events logged not dropped. Blocked by #22.
- **#28 — Memory & resource enforcement (AFK, security).** Per-guest linear-memory
  ceiling, fuel metering kills infinite loops, **WASI restricted (no fs, no
  net)**, clean host recovery after a kill. On Path B these are native wasmtime
  knobs (`StoreLimits`, `Config::consume_fuel`/epochs, a locked-down `WasiCtx`).
- **#29 — Hot-reload (AFK).** Watch modules; drain in-flight on old, route new to
  replacement, zero dropped; reject a corrupt replacement and keep serving the
  old. Blocked by #25.

**Phase 3 "done" (PRD checkpoint):** runtime loads the echo guest; echo calls
`query_graph("MATCH (p:Person) RETURN count(p)")` and gets **12** (via the count
quirk: project nodes / count rows); `FirmContext` flows HTTP→mesh→guest→SparrowDB;
a scheduled task fires on an interval; a slow guest is killed at its TTL.

---

## 8. First moves (suggested order)

1. Read the frozen PRD §Phase 3 + ADR-0001 + this doc.
2. `git submodule update --init --depth 1 crates/tachyon-mesh` (done locally, but
   a fresh clone needs it). Skim `core-host/src/host_core/` and `faas-sdk`.
3. **Probe wasmtime 45** in a scratch build; **attempt `core-host`
   `--no-default-features`**. Decide Path A vs B and **confirm with the operator.**
4. Write a short **ADR-0002 (Phase 3 runtime decision)** capturing the choice,
   the boundary serialization format (#22), and the runtime limits (#21).
5. Build #21 → #22 → #23 → #24 first (the security-critical spine), each as a
   tested vertical slice, before the scheduler/event-bus/hot-reload breadth
   (#25–#29).
6. Keep `query_graph` pinned to `GuardedStore`. Never let a guest touch the raw
   `Store` or set its own clearance.

---

## 9. Gotchas log (things that already cost time once)

- The vendored upstreams pin their **own** `rust-toolchain.toml`; a standalone
  `cargo build` inside a submodule uses a *different* toolchain than the workspace
  build. Don't be fooled when a submodule "builds fine" standalone.
- SparrowDB query results: prefer **inline pattern filters + un-aliased
  projections**; treat `WHERE`/`EXISTS`/`shortestPath`/`count`-over-path/bool as
  per ADR-0001.
- GitHub MCP connectivity has been intermittent; the issue tracker is the source
  of truth for acceptance criteria — re-read the live issue before closing it.
- Issues are still labeled `needs-triage`; Phases 1–2 did not relabel them. Align
  with the operator on whether to manage labels.
