# Handoff — post-PRD (after Phases 0–6)

> Read this with the frozen PRD extract and the ADRs in `docs/adr/`. The PRD's
> seven phases (0–6) are **all implemented and merged to `main`**. This document
> captures the repo-specific reality for whoever picks up the remaining follow-up
> work, and an important issue-tracker housekeeping note.

## 1. Where the project stands

All seven phases are done, green, and merged. The build is one workspace; the API
is the canonical **HTTP → Auth → Mesh → Guest → Graph** surface, with the three
business guests embedded in the binary.

| Layer | Crate | State |
| --- | --- | --- |
| Core | `kanbrick-core` | `FirmContext`, `ClearanceLevel`, ids, errors, graph vocab, host↔guest `abi`. |
| L3 Brain | `kanbrick-store` | Embedded SparrowDB: `Store`, typed schema, `query`, `Migrator`, `seed` (incl. `seed::load_str`). |
| L1 Guard | `kanbrick-auth` | JWT, Argon2id, `LoginService`, `require_clearance`, `ClearanceScope` (+ `PUBLIC_COMPANY_FIELDS`, `retain_rows`), `GuardedStore`, `AuditLog`, `ApiKeyService`. |
| L2 Nerves | `kanbrick-mesh` | wasmtime-45 runtime: `MeshRuntime` (registry, dispatch/invoke, hot-reload, `with_store`/`with_bus`), host imports `kbk_ctx_*`/`kbk_query_graph`/`kbk_emit_event`/`kbk_log`, `Scheduler`, `EventBus`. |
| L4 Map | `kanbrick-discovery` | graphify-libs analytics; composable `VisibilityScope` + additive `ProjectScope`; `DiscoveryCache`. |
| SDK | `kanbrick-guest-sdk` | typed `firm_context`/`query_graph`/`emit`/`log` + `guest_entrypoint!`. |
| API | `kanbrick-api` | Axum: `/login`,`/me`,`/admin`,`/health`,`POST /guests/{name}`; **embeds** the guests via `build.rs` + `include_bytes!`. |
| CLI | `kanbrick-cli` | `seed`, `set-password`. |
| Guests | `guests/{valuation,reporting,compliance}` | real `wasm32-wasip1` business modules; `guests/{echo,sdk-example}` are wasm-only fixtures. |

**The CI gate (keep it green every commit):**
```bash
git submodule update --init --depth 1 crates/sparrowdb   # required to build
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build  --workspace --all-features
cargo test   --workspace --all-features
scripts/build-guests.sh        # guests → wasm, < 10 MiB each
```
Toolchain pinned `1.94.1` + `wasm32-wasip1` (`rust-toolchain.toml`). Develop on a
fresh branch off `main`; one vertical slice per issue; don't open a PR unless
asked; **never put the model identifier in committed artifacts**.

> **PR keyword gotcha (learned the hard way — see §5):** to actually auto-close an
> issue, repeat the keyword per number: `Closes #38\ncloses #57`. A comma list
> (`Closes #1, #2`) or an en-dash range (`#30–#37`) does **not** close them.

## 2. How this project is built (match this)

1. **De-risk the upstream FIRST.** Every phase's hard part was the third-party
   crate, and the PRD was repeatedly wrong about it: SparrowDB's Cypher dialect
   (ADR-0001), "Ironclaw is a binary" (Phase 2), "Tachyon-Mesh is not a drop-in"
   (ADR-0002), "graphify-rs is a binary, not the org-graph lib the PRD assumed"
   (ADR-0003). Probe before designing.
2. **One vertical slice per issue, tested.** Native unit tests for pure logic;
   real-wasm integration tests through the mesh; seed-backed assertions.
3. **Flag PRD deviations loudly, proceed; bring one-way doors to the operator.**
   The operator made the HITL calls recorded in ADR-0003/0004/0005.
4. **Record one-way-door decisions in an ADR** (`docs/adr/`).

## 3. Landmines (things that already cost time)

- **SparrowDB dialect (ADR-0001).** Project un-aliased (`RETURN p.email`; aliasing
  a property → `Null`). `WHERE`/`count()`-over-path/`shortestPath`/parameterized
  `CREATE` are unreliable — **project nodes and count/compute in Rust**. Two
  same-named columns (`a.email, b.email`) collide under the row-mapper's prefix
  strip → use per-node queries. Booleans round-trip as `i64` 1/0.
- **`PUBLIC_DATA` (ADR-0005).** Company `company_id`/`name`/`segment` are public to
  every clearance; everything else (other company fields, personnel, financials)
  is gated. Enforced in `ClearanceScope::retain_rows`. Don't reintroduce a
  "company name is secret" assumption.
- **Host-authoritative identity.** A guest reads `FirmContext` only via `kbk_ctx_*`.
  Never inject identity into a request/guest payload (the API deliberately does
  not). `query_graph` is clearance-filtered + audited by `GuardedStore`.
- **Guest crate shape (ADR-0004).** Pure logic = native-testable `rlib`; the
  `#[cfg(target_arch = "wasm32")]` entrypoint does the SDK IO. Clearance rejection
  is a structured error response (the SDK turns `Err` into `{error,kind}`), not a
  trap.
- **Guest wasm is built by `build.rs`** in `kanbrick-mesh` and `kanbrick-api`
  (isolated target dir; wrappers/RUSTFLAGS stripped). Adding a guest = add it to
  both `GUESTS` lists + (for HTTP) `build_mesh` + `guest_min_clearance`.
- **`-D warnings` is real**, including `clippy::derivable_impls` (use
  `#[derive(Default)]` + `#[default]`) — both reporting and valuation hit it.
- **Test time / cold start.** wasmtime compiles each guest module on
  `register_module` (~2 s with Cranelift), so API/mesh tests that build a fresh
  runtime per test are slow (the API E2E ~50 s). The release build is ~6 min
  (marginally over the 5-min target). See the backlog (§4) for the fix.

## 4. Remaining work (prioritized)

### Genuinely outstanding
1. **#57 — per-project scopes + customizable per-project agents/skills** (operator
   request, HITL). The **enforcement primitive already exists**:
   `kanbrick_discovery::ProjectScope` (additive, composable `VisibilityScope`).
   What's left is the *lifecycle*: a request → approval → grant workflow,
   persistence (likely `(:ScopeRequest)`/`(:ProjectScope)`/`(:Skill)` nodes in
   SparrowDB), eligible-grantor rules (clearance threshold and/or
   `common_manager`), and binding per-project skills/agents to a granted scope.
   See issue #57's "Open design questions" — bring them to the operator.
2. **#38 — code-graph ingest** (deferred in Phase 4, HITL: **non-LLM/AST** chosen,
   ADR-0003 §6). Use `graphify-extract` (`collect_files`/`extract`) →
   `graphify-build::build_from_extraction` → `graphify-export::export_cypher`, then
   ingest into the same SparrowDB with code-ontology labels
   (Function/Module/Document + CALLS/IMPORTS/DEFINED_IN/REFERENCES). Those crates
   are **already declared** in `[workspace.dependencies]`; no manifest change.
   Watch: re-ingest must not duplicate nodes (use `MERGE`/inline, per ADR-0001).
3. **#53 — finish deployment artifacts** (env-gated, see §6). The self-contained
   binary is **done** (23 MB, embeds all guests, smoke-tested). Remaining: build +
   smoke the **Docker image** (`scripts/docker-release.sh`, needs a running Docker
   daemon) and the **fully-static musl** binary (needs `musl-tools`/`musl-gcc` +
   `rustup target add x86_64-unknown-linux-musl`).

### Hardening / optimization backlog (no open issue yet — file if pursued)
- **Cold-start / test speed:** precompile guests with `wasmtime` serialized
  modules (`Module::serialize`/`deserialize`) and embed those, instead of
  compiling on `register_module`. Cuts cold start well under 500 ms and speeds the
  API/mesh test suites.
- **Release build time:** add `sccache` / CI caching to get a clean release build
  under 5 min.
- **Token replay (#48 follow-up):** stateless bearer JWTs are replayable within
  TTL by design. Add server-side revocation (a `jti` deny-list or session
  tracking) if replay protection is required. Documented in `docs/SECURITY.md`.
- **Perf coverage (#49 follow-up):** add the 12-concurrent-user load harness and a
  steady-state memory (RSS) sampler. Batch the compliance guest's 12 per-person
  `REPORTS_TO` `query_graph` calls (it dominates its ~80 ms execution).
- **Docs review (#52, HITL):** a human should accuracy-review README /
  ARCHITECTURE / SECURITY / CONTRIBUTING.

## 5. Issue-tracker housekeeping (important)

The auto-close keyword gotcha (§1) means **#6–#52 are implemented and merged but
still show OPEN**, and #53 is partially done. The real state:

- **Implemented & merged — should be CLOSED:** #6–#37 **except #38**, and #39–#52.
  (Phases 1–6: store, auth, mesh, discovery, guests, testing/validation.) Close
  each with a note pointing at the merge (Phase PRs #55/#56/#58/#59/#60).
- **#53 — keep open**, scoped down to "Docker image build/smoke + musl static"
  (the binary half is done).
- **#38, #57 — keep open** (genuinely outstanding, above).

Before closing in bulk, spot-check a couple against the code so nothing is closed
prematurely. Future PRs: use `closes #N` **once per issue**.

## 6. Environment notes — what needs a local/CI machine

Nearly all remaining *development* (#57, #38, the backlog) works in the **remote
sandbox** — `cargo build`/`test`/`clippy`, the wasm target, and SparrowDB all
function here. Two things do **not** work in this sandbox and need a local machine
or a CI runner:

- **Docker image (#53):** the `docker` CLI is present but **the daemon is not
  running**, so `docker build` fails here. Run `scripts/docker-release.sh` where a
  Docker daemon is available.
- **Static musl binary (#53):** `musl-gcc`/`musl-tools` and the
  `x86_64-unknown-linux-musl` target are not installed here (and C deps like
  `ring`/SparrowDB need musl-gcc). Install them on a Linux host/CI and build there.

You do **not** need to clone locally just to direct the next agent — it can do the
code work remotely. Clone locally (or use a CI runner) only to **build/verify the
Docker image or the musl static binary** yourself.

## 7. First moves (suggested)

1. Read the PRD §Phase 4–6, ADR-0003/0004/0005, `docs/ARCHITECTURE.md`,
   `docs/SECURITY.md`. Confirm the workspace builds/tests green before touching
   anything.
2. Do the issue-tracker housekeeping (§5) so "remaining" is accurate.
3. Pick up **#57** (bring its design questions to the operator first — it's a
   security one-way door) and **#38** in parallel (independent).
4. Land the optimization backlog opportunistically (serialized modules is the
   highest-leverage: it fixes cold start *and* test speed).
