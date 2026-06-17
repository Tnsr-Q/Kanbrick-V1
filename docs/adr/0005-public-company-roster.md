# ADR 0005 — Public company roster: company name/segment are PUBLIC_DATA

- **Status:** Accepted
- **Date:** 2026-06-16
- **Context:** Phase 5 reporting guest (#43/#44). Amends the Phase 2 clearance model
  (#17, `kanbrick-auth::ClearanceScope`).
- **Deciders:** Phase 5 agent + operator (HITL: #44 per-tier field visibility).

## Context

#44 requires that **L1 sees company names** ("company names only — no financials
or personnel details"). The Phase 2 clearance model, however, makes an L1 caller
see **zero** companies through the host-filtered `query_graph` (an L1 manages no
companies, so `can_see_company` is false for all). A guest cannot bypass the
clearance filter, so it cannot surface those names for L1 — the two requirements
conflict.

## Decision (operator-approved)

**Company identity is `PUBLIC_DATA`.** The fields `company_id`, `name`, and
`segment` form a public *roster* visible to **every** clearance. Everything else
— all other company fields, every person/personnel field, and all financial data
(the Phase 5 `FinancialSnapshot` nodes) — remains clearance-gated. This recognises
reality: company names are public; financials and personnel are not.

### Enforcement (consistent across interfaces)

The single generic choke point `ClearanceScope::retain_rows` (used by
`GuardedStore::query_graph`, i.e. every guest query) classifies each result row:

1. **Person row** (has `email`): kept iff `can_see_person` — unchanged.
2. **Public roster row** (all projected keys ∈ `PUBLIC_COMPANY_FIELDS` =
   `{company_id, name, segment}`): kept for **everyone**.
3. **Company detail row** (has `company_id` plus any non-public field): kept iff
   `can_see_company` — unchanged.
4. **Otherwise** (a sensitive projection exposing no clearance key): **denied**,
   fail-closed — unchanged. So `RETURN c.description` is still denied for a
   non-privileged caller; only the public fields are ever freely readable.

The typed `retain_companies(Vec<CompanyNode>)` path is unchanged: `CompanyNode`
carries non-public fields, so it is *detail* and stays gated (L3 → its 5, L2/L1 →
0). The publicness is exactly and only the roster projection.

### Per-tier reporting dashboard shape (#44)

| Tier | Roster (names/segments) | Company detail (stakeholders, mgmt, counts) |
| --- | --- | --- |
| L5 / L4 | all 9 | all 9 |
| L3 | all 9 (public) | own segment only |
| L2 | all 9 (public) | assigned only |
| L1 | all 9 (public) | none (+ own activity) |

"#44 L3 sees only its segment's companies" is read as **detail** scoped to the
segment; the roster is public to all.

## Consequences

- `kanbrick-auth`: `retain_rows` gains the public-roster rule and a
  `PUBLIC_COMPANY_FIELDS` constant. Person and financial data stay fully gated;
  the fail-closed default holds for any sensitive projection.
- Tests that used `RETURN c.company_id, c.name` as a *gated* projection (mesh
  `host`/`guest_query`, the sdk-example guest) are updated to include a sensitive
  field (e.g. `description`) where they intend to exercise detail gating, and a
  new roster test asserts an L1/L2 caller sees all 9 names.
- The reporting guest (#43/#44) builds its dashboard on this: a public roster for
  all, with detail gated by `can_see_company`.
- Personnel are **not** public; only company identity is. Revisit only if a
  future requirement makes a person field public.
