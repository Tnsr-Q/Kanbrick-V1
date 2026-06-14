# ADR 0001 — SparrowDB Cypher capability matrix (vendored pin)

- **Status:** Accepted
- **Date:** 2026-06-14
- **Context:** Phase 1 (store) → informs Phase 2 (clearance filtering) and Phase 4 (discovery)
- **Pinned upstream:** `crates/sparrowdb` @ `82d85b7` (workspace version `0.1.22`)

## Context

The PRD's top risk is "SparrowDB Cypher dialect incompatibility." During Phase 1
we found the dialect is narrower than the seed data assumed, and an early flag of
mine ("SparrowDB cannot do variable-length paths") turned out to be **wrong** — it
was caused by testing with `count()` aggregated over a path rather than projecting
the reached nodes. This ADR records the **empirically verified** behavior of the
pinned build so later phases build on fact instead of guesswork. Every row below
was confirmed by running queries against the seeded 12-person firm graph.

## Verified capability matrix

### Works reliably — build on these

| Construct | Example | Notes |
| --- | --- | --- |
| Inline property filters in patterns | `MATCH (p:Person {role: 'CEO'})` | The reliable way to filter. Used throughout the seed and tests. |
| Variable-length traversal, **projecting nodes** | `MATCH (p)-[:REPORTS_TO*1..4]->(ceo:Person {role:'CEO'}) RETURN p.email` | Returns the reached node set. Bounded `*1..n` and unbounded `*` both work. This is the backbone for Phase 4 discovery. |
| Variable-length with `WHERE` on the **traversed subject** | `MATCH (sub)-[:REPORTS_TO*1..10]->(p {email:$x}) WHERE sub.clearance_level = 'L4' RETURN sub.email` | Filtering the var-length-bound variable works (returned the correct 4). |
| Inline terminal property filter on a var-length path | `MATCH (a {name:'Alice'})-[:LINK*1..2]->(b:Node {active:true})` | Upstream-tested; confirmed. |
| 1-hop relationship queries | `MATCH (p:Person)-[:MANAGES]->(c:Company {company_id:'JMTS'}) RETURN p.email` | `company_stakeholders('JMTS')` returns the correct 5 natively. |
| Multi-pattern `MATCH (a {..}), (b {..}) CREATE (a)-[:R]->(b)` | incl. cartesian (one side unlabeled) and edge properties | The supported write form for relationships. |
| `MERGE` with **inline** properties | `MERGE (p:Person {email:$e, full_name:$n, ...})` | The supported **parameterized** write path. |
| Legacy DDL | `CREATE CONSTRAINT ON (n:L) ASSERT n.p IS UNIQUE`, `CREATE INDEX ON :L(p)` | Uniqueness enforced; duplicate insert raises a violation. |

### Does NOT work / unreliable in this build — avoid or work around

| Construct | Observed behavior | Workaround |
| --- | --- | --- |
| `count()` / aggregation **directly over** a variable-length path | Returns an empty result (no rows) | Project the reached nodes and count rows in Rust. |
| `WHERE` filter on a **bare single-node** scan + property projection | Returns rows with **null** projections (does not filter); fails for `=`, `>=`, `>`, on both string and numeric properties | Use an **inline** pattern filter `{prop: value}` instead; for ranges/inequalities, post-filter in Rust over the projected set. |
| String **ordinal** comparison | `WHERE p.clearance_level >= 'L4'` returns 0 | Don't compare clearance as strings. Carry a numeric rank and/or filter in Rust (see Phase 2/4 guidance). |
| `EXISTS { (n)-[:R]->(:X) }` / `NOT EXISTS { ... }` | Returned null-laden / empty result sets against the firm graph (despite an upstream acceptance test) | Determine membership via 1-hop projection + Rust set operations. |
| Inline negation `NOT (a)-[:R]->()` | Not evaluated (returns 0) | Use 1-hop adjacency + Rust. |
| `shortestPath((a)-[:R*]->(b))` scalar | Executes but returned a wrong value (2 for a known 4-hop chain) | Derive path/length from the projected node chain in Rust. |
| Parameterized standalone `CREATE` / `MATCH...CREATE` | `execute_with_params` rejects it explicitly | Use `MERGE` with inline `$params`, or `MATCH...SET n.x = $v`. |
| `count(DISTINCT x)` | `DISTINCT` is an unexpected token | De-duplicate in Rust. |
| Fixed patterns of ≥ 3 relationship hops | Returned empty | Use variable-length `*` instead (which works). |

## Decision

1. **Single source of truth = SparrowDB.** We do **not** introduce a second,
   separately-synced graph (e.g. mirroring into graphify-rs for traversal). The
   variable-length capability removes the need, and a divergent second graph on
   the clearance-enforcement boundary (Phase 4 #36) would be a data-leak vector.

2. **Phase 4 org-discovery queries** (`reporting_path`, `span_of_control`,
   `org_neighborhood`, `common_manager`, `company_stakeholders`) are built as
   **native variable-length Cypher that projects nodes**, with a thin Rust layer
   in `kanbrick-discovery` for the narrow gaps (counting, path length,
   negation/"no reports").

3. **graphify-rs stays in its lane** — building the code/document graph and
   exporting `cypher.txt` into SparrowDB (#38). It is not the org-graph traversal
   engine.

4. **Clearance filtering (#36 / #17 / #18) is performed in Rust** over the node
   set SparrowDB returns, not via SparrowDB `WHERE` comparisons. SparrowDB's
   `WHERE`/comparison engine is unreliable in this build, and — more importantly
   — keeping the security-critical clearance decision in audited Rust is the
   safer design regardless. If/when we want DB-side pre-filtering, carry a
   **numeric `clearance_rank`** property on `Person` nodes (string ordinal
   comparison does not work) and verify the operator first.

## Consequences

- Phase 4 is **not blocked** and needs no dual-graph architecture.
- Query authors should prefer **inline pattern filters + variable-length node
  projection** and treat `WHERE`-clause filtering, `EXISTS`, `shortestPath`, and
  aggregation-over-paths as unavailable until re-verified.
- Revisit this ADR when the SparrowDB pin is bumped: several gaps (WHERE on bare
  scans, `EXISTS`, `shortestPath` value, `count` over paths) look like fixable
  engine bugs and may clear on a newer release, at which point some Rust
  workarounds can be retired in favor of native Cypher.

## How to re-verify

The findings were produced with short probe tests under `kanbrick-store/tests/`
that seed the firm graph via `Migrator::firm(...)` and run candidate queries
through `Store::query` / `Store::scalar_i64`. Re-run equivalent probes after any
SparrowDB pin bump and update the matrix above.
