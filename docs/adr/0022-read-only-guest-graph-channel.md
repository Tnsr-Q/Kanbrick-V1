# ADR 0022 — Read-only guest graph channel + fail-closed grant expiry

- **Status:** Accepted
- **Date:** 2026-07-02
- **Context:** Phase 16 (Governed Autonomy), slice **P16.1** (#149) — the security
  pre-work every later P16 slice stands on. PRD:
  `docs/prd/phase-16-governed-autonomy.md` (§1.1, §1.4, §5 P16.1; merged via #147).
  Builds on the audited read choke point (ADR-0001/#18/#24), scope grants
  (ADR-0007), and host-authoritative identity (ADR-0002/0016).
- **Deciders:** P16 agent + **operator** (the PRD review fixed the slice ordering;
  making the guest channel read-only is a one-way door for guest authors — the
  sanctioned mutation path becomes P16.3 proposals).

## Context

The Phase 16 verification pass found that the guest graph channel was a live
write channel: `GuardedStore::query_graph` audited the statement and
clearance-filtered **returned rows**, but passed the guest's arbitrary Cypher to
`Store::query` → `Store::execute_with` — the same execution path as host writes.
A guest could submit `MERGE` / `MATCH … SET` and it committed directly against
the firm graph, before and regardless of row filtering. The documented security
model (README, `docs/SECURITY.md`: guests read the graph "only through audited,
clearance-filtered host imports") describes reads; writes were never in the
contract. The three business guests are read+emit only, so nothing depended on
the hole.

Two adjacent findings landed in the same slice because P16.4 autonomy grants
make them load-bearing:

- `is_expired` treated an **unparseable** `expires_at` as *never expiring*
  (fail-open) — intolerable once autonomy promotion rides grant TTLs and grant
  lapse is the automatic sunset for unattended execution.
- The suspected "revoke cache gap" (the HTTP revoke path passing `cache: None`)
  needed verification, because P16.5's stop-latency story leans on revocation
  propagating to in-flight loops.

## Decision

1. **The guest graph channel is read-only, enforced fail-closed at the single
   choke point.** A `readonly` classifier in `kanbrick-auth` runs before any
   guest statement reaches the store: string literals, backtick identifiers,
   and comments are blanked (unterminated regions are refused outright), then
   any whole-word match against the Cypher write/DDL vocabulary
   (`CREATE MERGE SET DELETE DETACH REMOVE DROP FOREACH LOAD CALL`) refuses the
   statement with an `Unauthorized`-kind error. The gate lives inside
   `GuardedStore::query_graph`, so it covers **both** guest paths at once —
   in-process (`LocalHostServices`) and the executor split
   (`/internal/graph/query`). False positives are accepted by design: a bare
   keyword-shaped property (`n.set`) is refused; the documented workaround is a
   backtick-quoted identifier (`` n.`set` ``). The keyword set is deliberately
   wider than what the pinned SparrowDB dialect executes today (ADR-0001), so
   the gate stays closed if the engine grows support.

2. **Refusals are audited.** A refused statement records an audit entry under
   the caller with a `guest-query-refused:` marker prefix before the error
   surfaces, so the trail distinguishes an attempted write from an executed
   read (hash-only today; P16.2 adds structured action fields).

3. **The host remains the graph's only writer.** No propose/staging semantics
   are added here — a guest that needs to mutate state expresses it in its
   output payload, which P16.3 materializes as a host-written `(:Proposal)`
   with a schema-validated applier. This ADR closes the unsanctioned path;
   P16.3 opens the sanctioned one.

4. **Grant expiry fails closed.** `is_expired` now reads an unparseable
   `expires_at` as **expired**. A missing/blank expiry remains open-ended (an
   explicit grant shape; whether interactive grants should require TTLs is PRD
   §7.3, still the operator's call).

5. **The revoke "cache gap" is verified as not a defect — recorded, not
   fixed.** `authorize_skill` re-reads scope status from the store on every
   per-step gate; no cache sits in that path, so a revoke reaches an in-flight
   loop at its next step boundary. The API holds no `DiscoveryCache` (the
   `None` at the revoke handler is correct), and the `OrgGraphCache` on
   `AppState` serves only grantor-chain checks at approve/deny — never scope
   status. A regression test pins revoke → next `authorize_skill` fails, and
   the handler comment now states the invariant to preserve if a
   `DiscoveryCache` is ever wired into `AppState`.

## Consequences

- **Guests lose an undocumented capability, not a contracted one.** The three
  embedded guests are unaffected (verified by their existing read-path tests).
  A registry-activated guest that relied on smuggled writes breaks loudly at
  its next call — with an audited refusal explaining why.
- **P16.3 gets a truthful foundation**: "guests propose, the host commits" is
  now enforced, not aspirational, and the proposal flow becomes the *only*
  mutation path from guest logic.
- **P16.4 autonomy grants inherit a trustworthy sunset**: expiry can no longer
  silently fail open on a malformed timestamp, and revocation is proven to
  gate the next step just-in-time.
- The classifier is lexical, not a parser: it can refuse legitimate reads
  (keyword-shaped bare identifiers) and that is the accepted trade — every
  false positive has a backtick workaround; a false negative would be a
  security hole. If the dialect ever needs finer classification, tighten
  toward a parse-level allowlist, never toward narrowing the keyword scan.
