# ADR 0007 — Project-scope grant lifecycle (#57): SparrowDB-backed, dual-gate, additive, revocable

- **Status:** Accepted
- **Date:** 2026-06-18
- **Context:** Phase 4 follow-up (#57), filed from the #36 operator decision and
  ADR-0003 §5. Builds on the `ProjectScope` enforcement primitive (ADR-0003 §5),
  the clearance model (ADR-0005), the SparrowDB dialect (ADR-0001), and the
  discovery org-chart analytics.
- **Deciders:** Follow-up agent + **operator** (the four open design questions in
  #57 — persistence, eligible grantor, skill binding, lifetime/revocation — were
  answered directly by the operator; this ADR records those answers and the
  codebase adaptations needed to honour them).

## Context

`ProjectScope` (ADR-0003 §5) already enforces an **additive** grant on top of a
base `VisibilityScope`. #57 builds the **lifecycle** around it: request →
approval → grant → use, plus persistence, expiry, revocation, and per-project
skills. The operator supplied a reference design; it had to be adapted to two
hard codebase facts: the **SparrowDB dialect is narrow** (ADR-0001) and
**identity is host-authoritative** (a guest reads `FirmContext` via `kbk_ctx_*`;
identity is never injected into a payload).

## Decision (operator answers, as implemented in `kanbrick_discovery::grants`)

1. **SparrowDB is the source of truth.** Scopes/requests/skills are *business
   state* (they change daily, must be queryable and revocable at runtime), not
   configuration. Persisted as `(:ScopeRequest)`, `(:ProjectScope)`, `(:Skill)`
   nodes with `(:ProjectScope)-[:HAS_SKILL]->(:Skill)` edges. Rejected: config
   files (static, no audit, no runtime revocation), KV (not durable/schema'd),
   in-memory (ephemeral).

2. **Dual-gate grantor.** A grantor must hold clearance ≥ **L4** *and* be in the
   requester's **management chain** — unless they are an **L5 cofounder**
   (firm-wide override). The chain check reuses the in-memory `DiscoveryGraph`'s
   `ancestors()` rather than an (unreliable) variable-length `WHERE` Cypher query.
   `eligible_grantor()` is `clearance≥L4 && (in_chain || is_L5)`.

3. **Skill binding & execution.** A `Skill` is a graph node bound to a scope via
   `HAS_SKILL`, carrying `guest` + `required_clearance`. `authorize_skill()` is
   the runtime gate: scope ACTIVE & unexpired, caller **is the grantee**, caller
   clearance ≥ skill minimum — then it returns the **composed `ProjectScope`** the
   skill runs under. **Adaptation:** the reference design injected
   `_firm_context` into the guest payload; we do **not** — that would break the
   host-authoritative-identity invariant. Identity stays host-side; only the
   *composed scope* is handed to the host to enforce. Wiring this returned scope
   into the live mesh guest `query_graph` path is the remaining integration (see
   Consequences).

4. **Lifetime = expiry + revocation, both cache-invalidating.** Scopes carry an
   RFC3339 `expires_at`. `expire_due(now)` sweeps past-due ACTIVE scopes to
   EXPIRED; `revoke()` terminates one immediately (grantor or L5 only) and
   cascades its granted request to EXPIRED. Both invalidate the discovery cache.
   Enforcement does **not** wait for the sweep: `active_scope_for(now)` treats a
   past-`expires_at` scope as already gone, so a missed cron never leaks access.

## SparrowDB-dialect adaptations (ADR-0001)

The reference Cypher used `datetime()`, `OPTIONAL MATCH`, `WHERE`-filtered
variable-length paths, `LIST<>` properties and `CALL` procedures — none reliable
in the pin. As implemented:

- **Timestamps** are RFC3339 strings, compared in Rust (`chrono`).
- **Status/expiry filtering** is done in Rust over inline-matched rows
  (`MATCH (ps:ProjectScope {requester:$r, project:$p})`), never via `WHERE`.
- **Granted id-lists** are `|`-joined string properties (emails/codes contain no
  `|`), round-tripped exactly — instead of `LIST<>`.
- **Writes** use the blessed paths: parameterized node `MERGE`, parameterized
  `MATCH … SET a=$x, b=$y` (multi-assignment, SparrowDB SPA-157), and inline
  relationship `MERGE` for `HAS_SKILL` (ids are UUIDs — injection-safe).
- **Revocation cascade** is a second `MATCH … SET` (no `CALL`); token revocation
  is modelled as discovery-cache invalidation (there is no separate token store).

## Consequences

- New surface: `kanbrick_discovery::grants` (`ScopeGrants` +
  `ScopeRequest`/`GrantedScope`/`Skill` and `RequestStatus`/`ScopeStatus`). It is
  always-on (no `reqwest`/tree-sitter), unlike the `codegraph` module.
- The security-critical core is complete and tested (request → dual-gate approve/
  deny → additive enforcement through discovery → skill authorize → full audit →
  revoke/expire with cascade + cache invalidation), covering all of #57's
  acceptance criteria at the library layer.
- **Remaining integration (flagged, not yet built):** (a) an HTTP surface on
  `kanbrick-api` for request/approve/deny/revoke; (b) wiring the composed
  `ProjectScope` returned by `authorize_skill` into the mesh so a guest's
  `query_graph` runs under the grant (today the composed scope enforces at the
  discovery layer, which is where `ProjectScope` was designed to filter); (c) a
  scheduled `expire_due` sweep via the existing `Scheduler`. These are additive
  and do not change the model above.
- `ProjectScope` has `granted_persons`/`granted_companies` but no
  `granted_segments`; a segment grant should be expanded to its companies/persons
  at request time. Revisit if segment-level grants become first-class.
