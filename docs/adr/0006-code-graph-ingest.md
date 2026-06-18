# ADR 0006 — Code-graph ingest (#38): AST extraction, a three-class ontology, idempotent MERGE, feature-gated

- **Status:** Accepted
- **Date:** 2026-06-18
- **Context:** Phase 4 follow-up (#38, deferred in ADR-0003 §6). Builds on
  Phase 1 (`kanbrick-store`, SparrowDB dialect — ADR-0001) and the discovery
  crate's existing graphify integration.
- **Deciders:** Follow-up agent + operator (the binding HITL — *non-LLM/AST
  extraction* — was already taken in ADR-0003 §6; this ADR records the
  implementation decisions that landed it).
- **Upstream studied:** `graphify-extract`/`graphify-build`/`graphify-export`
  `0.8.0` (the same pin as ADR-0003) and the SparrowDB capability matrix
  (ADR-0001).

## Context

ADR-0003 §6 deferred #38 and fixed its one HITL one-way door: extraction is
**non-LLM (AST-only)** to honour the system's zero-external-dependency / no-network
philosophy. The remaining decisions are implementation shape: which graphify
APIs, what code ontology, how to ingest into SparrowDB idempotently, and how to
keep the deployed service lean. Following the project's *probe-before-designing*
habit, the graphify and SparrowDB internals were read first.

### What the upstreams actually do

- `graphify_extract::collect_files(root)` walks a source tree (already skipping
  `.git`/`target`/`node_modules`/`vendor`/`venv`); `extract(&paths)` runs the
  **Pass-1 AST** extractor only (tree-sitter where a grammar is linked, else a
  regex fallback). The Pass-2 *semantic* (Claude API) path is a **separate**
  function `extract` never calls — so `extract` makes no network call.
- graphify emits a rich node set (`File`, `Function`, `Method`, `Struct`,
  `Enum`, `Trait`, `Class`, `Interface`, `Module`, `Namespace`, `Package`, …)
  and **lowercase** relations (`defines` = container→entity, `calls`, `imports`,
  `uses`, `implements`).
- `graphify_export::export_cypher` writes Neo4j-style `CREATE` statements that
  reference variables **across** statements (`CREATE (a…); … CREATE (a)-[…]->(b)`)
  and is **not idempotent**. SparrowDB's loader executes one statement per call,
  so the raw export is **not** directly ingestible.
- SparrowDB (ADR-0001) supports **parameterized node `MERGE`** and, via the
  non-parameterized path, **relationship `MERGE`** (`MATCH … MERGE (a)-[:R]->(b)`,
  idempotent — SparrowDB test SPA-233). The parameterized path does **not** accept
  a relationship `MERGE`.

## Decision

1. **Pipeline = graphify library sub-crates, AST only.**
   `collect_files` → `extract` → `build_from_extraction` (drops dangling edges),
   in `kanbrick_discovery::codegraph`. No LLM/network (ADR-0003 §6).

2. **A three-class / four-relation ontology, with the precise kind preserved.**
   The issue prescribes classes `Function`/`Module`/`Document` and relations
   `CALLS`/`IMPORTS`/`DEFINED_IN`/`REFERENCES`. graphify's richer types are folded
   onto these, and the exact graphify kind is stored on every node as `kind`
   (so nothing is lost):

   | Ontology label | graphify `NodeType`(s) |
   | --- | --- |
   | `Function` | Function, Method, Struct, Enum, Trait, Class, Interface, Constant, Variable |
   | `Module` | Module, **File**, Namespace, Package |
   | `Document` | Concept, Paper, Image |

   `File → Module` is the key call: graphify's `defines` edges originate from the
   `File` node, so mapping files to modules is what makes the acceptance query
   `(:Function)-[:DEFINED_IN]->(:Module)` return rows. **Flagged deviation:** the
   coarse fold lands a `struct` under `Function` as a "named code definition";
   `kind` carries the precise type for anyone who needs it. This is the schema
   the #38 HITL note asked a human to confirm — revisit if a richer ontology is
   wanted.

   Relations: `defines` → `DEFINED_IN` (**reversed** to entity→container);
   `calls` → `CALLS`; `imports` → `IMPORTS`; `uses`/`implements`/other →
   `REFERENCES`.

3. **Ingest is idempotent and dialect-correct, not the raw export.** Nodes are
   written with parameterized node `MERGE` (keyed by the deterministic `make_id`,
   whose other properties are functionally determined by that id, so a re-run is
   a no-op). Edges are written with inline relationship `MERGE`
   (`MATCH … MERGE (a)-[:R]->(b)`); the inlined values are `make_id` outputs
   (`[a-z0-9_]`), so the inline form is injection-safe. `export_cypher` is still
   offered (and exercised) as the **inspectable artifact** the issue asks for —
   it is *not* the ingest path.

4. **Feature-gated (`codegraph`), off by default.** `graphify-build`/`-extract`
   pull a tree-sitter / `reqwest` dependency tree. To keep the deployed API
   network-free (ADR-0003 §6), the module and those deps are optional behind the
   non-default `codegraph` feature on `kanbrick-discovery` (re-exported by
   `kanbrick-cli` for the `code-ingest` command). CI exercises it via
   `--all-features`; `cargo tree` confirms the default `kanbrick-api`/`-cli`
   builds link neither `graphify-extract` nor `reqwest`.

## Consequences

- New surface: `kanbrick_discovery::codegraph` (`extract_code_graph`,
  `ingest_code_graph`, `ingest_from_source`, `export_cypher`, mapping helpers)
  and a `kanbrick-cli code-ingest --root <dir> [--db <dir>] [--export <dir>]`
  command (feature `codegraph`).
- The code graph and firm graph share one SparrowDB; a re-ingest of unchanged
  source does not duplicate nodes or edges (covered by tests, and smoke-verified:
  `kanbrick-store` → 78 functions / 45 modules / 596 edges, firm seed intact).
- The deployed service keeps the zero-network property; only the dev/admin CLI
  (built with `--features codegraph`) carries the extractor.
- Revisit the ontology if a human wants struct/trait/enum as first-class labels
  rather than folded into `Function`, or if graphify ships richer relations.
