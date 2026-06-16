# Phase 4 Handoff — Discovery: Graph Analysis (graphify-rs, L4 "Map")

> **Read this together with the frozen PRD** (the operator will give it to you).
> The PRD tells you *what* Phase 4 must deliver (its deliverables + checkpoints).
> This document tells you *the repo-specific reality the PRD does not know* — the
> state of the code after Phases 0–3, the graphify-rs unknowns, the SparrowDB
> landmines that still bite when you read the graph, the security thread you must
> not break, and the exact way Phases 0–3 were built so Phase 4 matches.

**Scope:** the Phase 4 issues — **start at #30** (Phase 3 was #21–#29); confirm the
exact range and the authoritative acceptance criteria against the live issue
tracker / frozen PRD. Develop on **the branch the operator specifies**. Do **not**
open a PR unless asked. Do **not** push to any other branch.

---

## 1. Where the project stands

Phases 0–3 are **done, green, and merged to `main`**. The build is one workspace.

| Layer | Crate | State |
| --- | --- | --- |
| Core | `kanbrick-core` | `FirmContext`, `ClearanceLevel` (L1<…<L5), `Status`, id newtypes, `Error`/`ErrorKind`, graph label vocab, and the **`abi` module** (host↔guest ABI: `HostFunctions`/`GuestModule` traits + `GraphQuery`/`GraphRows`/`Event`/`GuestRequest`/`GuestResponse`/`LogLevel`, all serde/JSON). |
| L3 Brain | `kanbrick-store` | Embedded SparrowDB wrapper: `Store` (open/close/checkpoint), `Params`, typed `query::<T>` / `query::<serde_json::Value>` / `scalar_i64`, `schema` (`PersonNode`/`CompanyNode`/`SegmentNode`), `Migrator`, `seed`. |
| L1 Guard | `kanbrick-auth` | JWT, Argon2id, `LoginService`, `require_clearance`, **`ClearanceScope`** (incl. `retain_rows` — the fail-closed generic clearance filter), **`GuardedStore`** (audited + clearance-filtered reads, incl. generic `query_graph`), `AuditLog`, `ApiKeyService`. |
| L2 Nerves | `kanbrick-mesh` | **Phase 3 — DONE.** wasmtime-45 WASM runtime: `MeshRuntime` (registry, `dispatch`/`invoke`, hot-reload), `MeshHost`, `EventBus`, `Scheduler` (timeout, per-guest concurrency, recurring + event triggers, retry). See ADR-0002. |
| L4 Map | **`kanbrick-discovery`** | **Phase 4 target.** Currently a trivial scaffold (`DiscoveryEngine::is_ready() -> false`). |
| API | `kanbrick-api` | Axum 0.8: `POST /login`, `GET /me`, clearance-gated `GET /admin`; structured 401/403. lib + bin. |
| Guests | `guests/{valuation,reporting,compliance}` | Lib stubs; become real `wasm32-wasip1` guests in Phase 5. `guests/echo` (Phase 3 fixture) is the reference for the real guest shape. |

**Build / test / lint (all must stay green — this is the CI gate):**
```bash
git submodule update --init --depth 1 crates/sparrowdb            # required to build
cargo build  --workspace --all-features
cargo test   --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```
Toolchain is pinned in `rust-toolchain.toml` (**1.94.1**). `graphify-rs = "0.8"` is
already declared in `[workspace.dependencies]` but **nothing uses it yet** — Phase 4
is where it gets wired into `kanbrick-discovery`.

---

## 2. How this project is built (match this exactly)

These conventions are why Phases 1–3 went cleanly. Follow them.

1. **De-risk the upstream FIRST, before writing any wrapper code.** For each phase
   the hard part is the third-party crate. *Before* designing, confirm with a
   throwaway probe (`kanbrick-discovery/tests/zz_probe.rs`, delete after): does
   `graphify-rs 0.8` resolve and build under our toolchain? What is its *real* API
   (read the docs.rs/source, not the README)? What graph type does it want, and
   how do you get firm data into it? This habit caught the SparrowDB dialect gaps
   and the Ironclaw/Tachyon-Mesh "it's not a drop-in library" surprises early.
2. **One vertical slice per issue, in dependency order.** Respect each issue's
   "Blocked by". Build → test → `clippy -D warnings` → `fmt` green at each step.
3. **Tests prove the issue's acceptance criteria.** Each issue has an explicit
   checklist; write tests hitting each bullet. Seed-backed integration tests read
   `seed/kanbrick_seed_data.cypher` via `Migrator::firm(...)` (see
   `kanbrick-auth/src/scope.rs` tests for the exact pattern, incl. the real names
   like Tyler/Tracy and the segment company codes).
4. **Record one-way-door decisions in an ADR.** `docs/adr/` — write
   **ADR-0003** for the Phase 4 graphify-rs integration choice (see §3) and the
   clearance-over-analytics decision (see §5). Precedent: ADR-0001 (SparrowDB
   Cypher), ADR-0002 (Phase 3 runtime).
5. **Flag deviations from the PRD loudly, then proceed.** The PRD has repeatedly
   been wrong about the upstreams. When you must deviate (a forced technical
   correction), do it, call it out in the commit + your summary, and don't block
   waiting for permission. *But* genuine one-way doors (see §5) get brought to the
   operator before you build on them.
6. **Commit cadence:** small, reviewable commits per slice; messages end with the
   session URL line already in git history. **Never push to a different branch;
   never open a PR unless explicitly asked.** Do **not** put the model identifier
   in any committed artifact.

---

## 3. The graphify-rs reality (the crux of Phase 4)

graphify-rs is a **crates.io** crate (not a vendored submodule — `crates/` has only
ironclaw, sparrowdb, sparrowdb-ontology, tachyon-mesh). It is declared as
`graphify-rs = "0.8"` in the workspace deps and is **edition 2024** (this is the
reason the toolchain was bumped to ≥1.85 back in Phase 0/1).

**Day-one probe — answer these before designing anything:**

```bash
# Does graphify-rs 0.8 actually resolve and build in our workspace on 1.94.1?
# (edition 2024 needs rustc >= 1.85 — we're fine — but verify the *minor* it pulls.)
# Add it to kanbrick-discovery/Cargo.toml, `cargo build -p kanbrick-discovery`.
```

Then read its real API and decide the **integration shape** (this is the
Phase-3-Path-A/B-equivalent fork; it belongs in **ADR-0003**):

- graphify-rs is almost certainly its **own in-memory graph** with analysis
  algorithms (centrality, shortest path, community/clustering, traversal). It does
  **not** know about SparrowDB. So the central question is the **data-loading
  boundary**: you will **read the firm graph out of SparrowDB and build a
  graphify graph from it**, run the analysis in-memory, and map results back to
  firm types (`PersonId`/`CompanyId`/`SegmentCode`, emails, company codes).
- Confirm: how are nodes/edges added? Are node ids arbitrary (so you keep a
  `graphify-node-id ↔ PersonId/CompanyId` map)? Which algorithms does 0.8 actually
  ship vs. what the PRD assumes (shortest path, betweenness/PageRank-style
  centrality, community detection)? Build a `DiscoveryGraph` wrapper in
  `kanbrick-discovery` that owns the graphify graph + the id mapping, so the rest
  of the crate speaks firm types, never graphify's internals (wrap the ABI in
  stable kanbrick types — same playbook as Phase 3 wrapping wasmtime).

**Network note:** this environment may be network-restricted. Confirm graphify-rs
0.8 is fetchable / already in the cargo registry cache (`~/.cargo/registry`)
before assuming `cargo build` will pull it. If it is not reachable, that is an
operator conversation, not something to work around silently.

---

## 4. SparrowDB landmines that *will* bite Phase 4

You load the firm graph by running Cypher against SparrowDB, so Phase 1's quirks
apply. **Read `docs/adr/0001-sparrowdb-cypher-capabilities.md` in full.** The ones
that matter when reading nodes/edges for analysis:

- **Variable-length traversal works** (`-[:REPORTS_TO*1..n]->`) — *project nodes*,
  never `count()` over a path. (`ClearanceScope::resolve` already relies on this.)
- **Don't alias bare-node projections** (`RETURN p.x AS y` returns `Null`). Project
  un-aliased; the store's row-mapper strips the `p.` prefix onto struct fields. For
  generic loads use `store.query::<serde_json::Value>(...)` (keys are the stripped
  column names).
- **`WHERE` on a bare single-node scan is unreliable** (null rows). Prefer inline
  pattern filters (`MATCH (p:Person {email: $e})`).
- **Booleans round-trip as integers** — store/read flags as `i64` 1/0.
- **Parameterized writes use `MERGE` (inline props) or `MATCH … SET x = $v`** —
  parameterized `CREATE` is rejected. (Phase 4 is mostly reads; relevant only if
  you persist computed metrics back as node properties.)
- The edges you'll likely traverse: `(:Person)-[:REPORTS_TO]->(:Person)`,
  `(:Person)-[:MANAGES]->(:Company)`, `(:Company)-[:IN_SEGMENT]->(:Segment)` (check
  `kanbrick-core::schema` `EdgeLabel`/`NodeLabel` and the seed for the exact set).

---

## 5. The security thread (do not break it) — and the Phase 4 one-way door

Phases 1–3 built and routed through the enforcement; Phase 4 must not route around
it. **This is the decision to bring to the operator and record in ADR-0003:**

> **Graph analytics vs. clearance.** Many graph algorithms (centrality, community
> detection, global shortest paths) are only meaningful over the **whole** graph,
> which conflicts with per-caller clearance filtering. You must decide, explicitly:
> compute over the full graph under a **system/privileged identity** and then
> **filter the *exposed results* to the requesting caller's `ClearanceScope`** (the
> recommended shape — analysis is privileged, *answers* are scoped), **vs.** compute
> only over each caller's visible subgraph (cheaper to reason about, but metrics
> differ per caller and are often meaningless). Pick one, justify it, write it down
> *before* building the query surface.

Tools already built for you to do this correctly:

- **`kanbrick_auth::ClearanceScope`** resolves a caller's visibility
  (`can_see_person(email)`, `can_see_company(company_id)`, `sees_all` for L4/L5),
  and **`ClearanceScope::retain_rows(Vec<serde_json::Value>)`** is the fail-closed
  generic filter (person rows keyed by `email`, company rows by `company_id`;
  unfilterable projections denied for non-L4/L5). **Reuse it to scope discovery
  output** — don't reinvent clearance logic.
- **`kanbrick_auth::GuardedStore`** is the audited, clearance-filtering read path
  (incl. generic `query_graph(GraphQuery) -> GraphRows`). If a discovery query is
  per-caller, go through this. If you load the full graph for global analysis, do
  it under a privileged context (see `ApiKeyService` — service identities return a
  `FirmContext` with `email = "service:<name>"`, `roles = ["service"]`) and **audit
  the load**, then scope the answers.
- **`FirmContext` is host-authoritative** (Phase 3, #23): never let a caller
  supply/forge identity or clearance. Discovery results that name people/companies
  must be filtered to what the caller may see.

**Net rule:** a discovery answer must never reveal a node/edge/metric the caller
could not have seen via a normal clearance-filtered query.

---

## 6. Toolchain, dependencies, CI

- **No new submodule.** graphify-rs is a crates.io dependency. CI
  (`.github/workflows/ci.yml`) requires only `crates/sparrowdb`; **do not** add a
  submodule step for Phase 4. (The other vendored upstreams remain excluded from
  the build graph.)
- **Toolchain stays 1.94.1.** graphify-rs is edition 2024 (needs ≥1.85) — already
  satisfied; you should **not** need a bump. If graphify-rs 0.8 transitively
  demands something newer, that's a flagged decision (re-verify the whole workspace
  builds, highest-requirement-wins, as the `rust-toolchain.toml` comment documents).
- **CI gates:** `fmt --all --check`, `clippy --workspace --all-targets --all-features
  -- -D warnings`, `build`, `test` — all green, every commit. `-D warnings` is real;
  an unused `mut` or a `too_many_arguments` will fail CI (Phase 3 hit both).
- **Targets:** `wasm32-wasip1` is installed (for the guests). Phase 4 is host-side
  Rust; no wasm target needed unless you also ship discovery as a guest (that's
  Phase 5's job — see §7).

---

## 7. Issue-by-issue guidance

Read the **live issues / frozen PRD** for the authoritative checklist; the shape
below is inferred from the L4 "Map" goal and may not match the exact numbering.
Build in dependency order, each a tested vertical slice.

Likely Phase 4 deliverables (confirm against the PRD):
- **Load the firm graph into graphify** — a `DiscoveryGraph` that reads
  persons/companies/segments + their edges from SparrowDB and builds the graphify
  structure, keeping a `graphify-id ↔ firm-id` map. Tested against the seed
  (correct node/edge counts; the org chart shape from `kanbrick-store`'s
  `org_chart` tests is a good oracle).
- **Shortest reporting-path / reachability** between two people (the placeholder in
  the current `DiscoveryEngine` doc) — "who connects A to B", "chain of command".
- **Influence / centrality ranking** over people or companies (whatever graphify
  0.8 ships; don't promise an algorithm it doesn't have).
- **Community / segment-cluster detection** or similar grouping.
- **Recommendation-style queries** if the PRD asks (e.g. "related companies").
- **Clearance-scoped result surface** (per §5) and **audit** for privileged loads.
- Possibly **API exposure** (`kanbrick-api` endpoints) or leaving consumption to
  Phase 5/6 — the PRD decides; don't assume.

Replace the scaffold: `kanbrick-discovery/src/lib.rs` currently has
`DiscoveryEngine::is_ready() -> Ok(false)`. The real engine + its modules go here.
Keep results in **firm types** (`PersonId`, `CompanyId`, `SegmentCode`, emails),
not graphify internals, so Phase 5/6 consume a stable surface.

**Phase 4 "done":** match the PRD's checkpoint (the discovery placeholder names
"shortest reporting-path queries"). Whatever the checkpoint, it should run over the
**seed** graph and return a correct, clearance-respecting answer end to end.

---

## 8. First moves (suggested order)

1. Read the frozen PRD §Phase 4 + ADR-0001 (SparrowDB) + this doc. Skim ADR-0002 for
   how Phase 3 wrapped an upstream in stable kanbrick types.
2. `git submodule update --init --depth 1 crates/sparrowdb` (fresh clone needs it).
   Confirm the workspace builds/tests green *before* you touch anything.
3. **Probe graphify-rs 0.8** (§3): add the dep, build, read its real API on real
   data. Decide the integration shape.
4. **Bring the clearance-over-analytics decision to the operator** (§5) and write
   **ADR-0003** (graphify integration + the clearance decision + any PRD deviation).
5. Build the `DiscoveryGraph` loader first (everything else needs it), then the
   analysis slices in dependency order — tested against the seed at each step.
6. Keep every graph read clearance-aware: scope answers with `ClearanceScope` /
   route per-caller queries through `GuardedStore`. Never expose a node a caller
   couldn't see normally.

---

## 9. Gotchas log (things that already cost time once)

- **The vendored upstreams pin their own `rust-toolchain.toml`;** a standalone
  `cargo build` inside a submodule uses a *different* toolchain than the workspace
  build. Don't be fooled when a submodule "builds fine" standalone.
- **SparrowDB query results:** prefer inline pattern filters + un-aliased
  projections; treat `WHERE`/`EXISTS`/`shortestPath`/`count`-over-path/bool as per
  ADR-0001. (If graphify gives you real shortest-path, you no longer need
  SparrowDB's — load the graph and compute in graphify.)
- **`-D warnings` is strict:** Phase 3 was bitten by an unused `mut` (after a
  method went `&mut self` → `&self`) and by `clippy::too_many_arguments`. Bundle
  args or `#[allow(...)]` with justification; drop needless `mut`.
- **Debug-build integer-overflow panics are real:** Phase 3 hit
  `u64::MAX` overflow in `set_epoch_deadline` once a background ticker advanced a
  counter. If you do arithmetic on accumulating counters/ids, use saturating/clamp.
- **GitHub/issue connectivity has been intermittent;** the issue tracker is the
  source of truth for acceptance criteria — re-read the live issue before closing.
- **Reusable Phase 3 surface you may want:** `kanbrick-mesh` (`MeshRuntime`,
  `Scheduler`, `EventBus`) — if Phase 4/5 wants discovery to run as a scheduled or
  event-triggered guest, the runtime is already there; the host↔guest ABI lives in
  `kanbrick_core::abi`. `ClearanceScope::retain_rows` is the ready-made output
  filter.
