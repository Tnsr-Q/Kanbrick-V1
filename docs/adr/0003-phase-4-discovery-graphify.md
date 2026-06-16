# ADR 0003 — Phase 4 Discovery: integrate graphify via its library sub-crates; analytics are privileged, answers are scoped

- **Status:** Accepted
- **Date:** 2026-06-16
- **Context:** Phase 4 (Discovery / "Map", L4) — issues #30–#38. Builds on
  Phase 1 (SparrowDB / `kanbrick-store`) and Phase 2 (clearance / `kanbrick-auth`).
- **Deciders:** Phase 4 agent + operator (HITL: #30 adapter architecture, #36
  clearance-vs-analytics, #38 extraction mode are one-way doors).
- **Upstream studied:** `graphify-rs` 0.8.0 (crates.io) and its sub-crates
  `graphify-core`/`graphify-analyze`/`graphify-cluster`/`graphify-export`
  /`graphify-build`/`graphify-extract`, all `0.8.0`, edition 2024.

## Context

The PRD names **graphify-rs** as the graph-analysis engine and assumes it is "its
own in-memory graph with analysis algorithms (centrality, shortest path,
community detection)" that we load SparrowDB data into. Following the project's
de-risk-the-upstream-first habit (Phase 1 SparrowDB dialect, Phase 2 "Ironclaw is
a binary", Phase 3 "Tachyon-Mesh is not a drop-in library"), we probed the real
crate before designing. The PRD's assumption is **wrong** in two ways.

### What graphify-rs 0.8 actually is

1. **`graphify-rs` is a CLI binary, not a library.** Its `Cargo.toml` declares
   `[[bin]]` with `autolib = false` and ships no `lib.rs` — depending on
   `graphify-rs` gives you an executable (and a tokio/clap/tree-sitter dependency
   tree), not a callable API. The reusable functionality is split across its
   **library sub-crates**, which the binary itself consumes:

   | Sub-crate | Provides |
   | --- | --- |
   | `graphify-core` | `KnowledgeGraph` (backed by `petgraph::StableGraph<_,_,Undirected>`), `GraphNode`/`GraphEdge`/`NodeType` model |
   | `graphify-analyze` | `pagerank`, `god_nodes` (degree centrality), `detect_cycles`, `community_bridges`, embeddings |
   | `graphify-cluster` | community detection |
   | `graphify-export` | `export_cypher`, `export_json`, `export_graphml` |
   | `graphify-build` / `graphify-extract` | source-tree (tree-sitter AST) → `KnowledgeGraph` |

2. **It is a code-analysis tool, not an org-graph algorithm library.** Its
   purpose is "transform code, docs, papers into queryable graphs" (tree-sitter
   AST → knowledge graph). It ships `pagerank`/`god_nodes`/`cluster`, but **not**
   the org-specific algorithms the PRD's Phase 4 deliverables name (shortest
   reporting path, lowest-common-manager, span-of-control). Its `KnowledgeGraph`
   is **undirected**, so even its centrality reflects undirected structure.

### Probe evidence (run 2026-06-16)

| Probe | Result |
| --- | --- |
| `cargo info graphify-rs` | `graphify-rs 0.8.0`, `[[bin]]`, edition 2024, rust-version 1.85, keywords `knowledge-graph/code-analysis/ast/tree-sitter/mcp`. |
| Read `examples/custom_graph.rs` | Library usage is `graphify_core::graph::KnowledgeGraph` + `add_node`/`add_edge`, then `graphify_analyze::{pagerank,god_nodes}` — exactly the "build a graph by hand, then analyze" shape we need for the firm graph. |
| `cargo build -p kanbrick-discovery` on **1.94.1** with `graphify-core/analyze/cluster/export` | **Builds clean**, ~11s. Pulls `petgraph 0.6.5`; **no toolchain bump**, no tokio/tree-sitter (those live in `build`/`extract`, used only by #38). |
| Throwaway runtime probe (3-node org) | `pagerank` ranked the *middle* of a reporting chain highest, not the CEO sink — confirming the backing graph is **undirected** and directed org semantics must be computed by us from the edge payload. |

## Decision

1. **Depend on the graphify *library sub-crates*, not the `graphify-rs` binary.**
   `[workspace.dependencies]` swaps `graphify-rs = "0.8"` for `graphify-core`,
   `graphify-analyze`, `graphify-cluster`, `graphify-export` (and
   `graphify-build`/`graphify-extract`, reserved for #38). **PRD deviation,
   flagged.** This mirrors Phase 2 (build on Ironclaw's *primitives*) and Phase 3
   (build on `wasmtime`, not the `tachyon-mesh` binary).

2. **#30 adapter = full in-memory copy (operator-approved).** `DiscoveryEngine::
   from_store` reads every Person/Company/Segment and their REPORTS_TO/MANAGES/
   BELONGS_TO_SEGMENT edges out of SparrowDB and builds **one** `KnowledgeGraph`
   plus firm-typed indices. At 12-person / 9-company seed scale a full copy is
   trivial and turns every query into an in-memory computation. Node ids are
   namespaced (`person:<email>`, `company:<code>`, `segment:<code>`) so the
   graphify-id ↔ firm-id mapping is total and unambiguous. The firm `NodeType`
   has no Person/Company/Segment variant, so the firm kind is carried in the
   node's `extra["kind"]`; firm-typed results (`PersonId`/`CompanyId`/
   `SegmentCode`) never expose graphify internals.

3. **Org algorithms are ours; graphify supplies the container + undirected
   influence.** `reporting_path` (shortest chain), `span_of_control`,
   `org_neighborhood`, `common_manager` (LCA) are computed over the directed
   REPORTS_TO adjacency we derive from each `GraphEdge.source/target/relation`
   (the petgraph backing is undirected, but every edge stores its own
   direction). `graphify_analyze::pagerank`/`god_nodes` and `graphify_cluster`
   back the *influence/centrality* surface (handoff §7), documented as
   **undirected** structural importance.

4. **#36 clearance model: analytics are privileged, answers are scoped
   (operator-approved, Option A).** Global graph metrics (centrality, shortest
   paths) are only meaningful over the whole graph, so analysis runs over the
   **full** graph under a privileged/system identity (the full-graph load is
   audited via `kanbrick_auth::AuditLog`). The **exposed results** are then
   filtered to the caller's visibility. The filter never leaks a node the caller
   could not have seen via a normal clearance-filtered query:

   | Result shape | Scoping rule |
   | --- | --- |
   | Path (`reporting_path`) | **All-or-nothing** — returned only if every person on it is visible; otherwise `None` (no partial paths → no gaps that reveal hidden nodes). |
   | Node list (`company_stakeholders`, `org_neighborhood`, segment persons) | Filtered to visible nodes; the *subject* (company/segment/person) must itself be visible. |
   | Scalar metric (`span_of_control`) | Answered only for a **visible subject**; the metric is the true global value (an aggregate, naming no hidden individual). |
   | Single node (`common_manager`) | Returned only if the result **and** both subjects are visible. |

5. **The visibility scope is an extensible, composable abstraction
   (operator-directed).** Rather than hard-wiring the static L1–L5 ladder,
   discovery filters through a `VisibilityScope` trait. `kanbrick_auth::
   ClearanceScope` is one implementation (static clearance). A **`ProjectScope`**
   composes *on top of* a base scope with **additive** grants
   (`granted_persons`/`granted_companies`/`granted_segments`): an employee can be
   granted extra, project-scoped visibility without changing their clearance, and
   a grant can only **add** visibility, never remove the base — so it cannot be
   used to escalate beyond what was explicitly granted. This is the foundation for
   per-project, employee-customizable AI agents/skills; the **request → approval →
   grant workflow** and persisted project-scope/skill entities are tracked
   separately (new issue, see Consequences) — this ADR fixes only the *scope
   abstraction* discovery enforces against.

6. **#38 (code-graph ingest) deferred (operator-approved).** This session ships
   #30–#37. When #38 lands it will use `graphify-extract`/`graphify-build`/
   `graphify-export::export_cypher` with **non-LLM (AST-only)** extraction by
   default, to honour the system's zero-external-dependency philosophy (no LLM
   API / network). `graphify-build`/`graphify-extract` are pre-declared in the
   workspace deps so #38 needs no manifest change.

## Consequences

- `graphify-core/analyze/cluster/export` gain real consumers in
  `kanbrick-discovery`; **no toolchain or CI submodule change** (Phase 4 is
  host-side Rust; no `crates/` submodule beyond `sparrowdb`).
- Security spine intact: the per-caller filter reuses `kanbrick_auth::
  ClearanceScope`'s `can_see_*`; the privileged full-graph load is audited;
  `FirmContext` stays host-authoritative. A discovery answer can never reveal a
  node/edge/metric the caller could not see normally.
- **New issue filed** for the employee-requestable, per-project scope + the
  customizable per-project skills/agents workflow (operator request during #36).
  `ProjectScope` here is the enforcement primitive it will build on.
- Revisit if graphify ships a directed graph or native shortest-path in a later
  release (we could drop our hand-rolled traversal), or if #38's code graph wants
  a richer ontology than AST extraction provides.
