# ADR 0015 — Tenancy topology: per-workstation control plane + central approval queue

- **Status:** Accepted
- **Date:** 2026-06-25
- **Context:** P8.7 (#99), **Phase 8 — Upstream De-Risk** (#79), L5 Cockpit
  program (#77). Extends ADR-0008's single-writer invariant to multi-tenancy;
  builds on ADR-0007 (ScopeGrants).
- **Deciders:** operator (tenancy topology is an operator decision) + P8 de-risk
  agent.

## Context

ADR-0008 makes SparrowDB **single-writer** and the control plane (CP) the **single
write point**. Multi-tenancy (req 7) and cross-user lead approvals (req 2.4) must
respect that. The operator chose **per-workstation CP + a central approval
queue**: each Cockpit runs its own `kanbrick-api` CP (its writes are local), and
cross-user scope / budget **approvals serialize through one shared central queue**.

## Decision

- **Queue tech.** V1 uses a small **CP-hosted queue service**: a central
  ("tenant-0") CP exposing an internal approval-RPC surface that **reuses the
  `caps.rs` / `internal.rs` `x-kanbrick-internal-token` fail-closed pattern**
  (ADR-0008). **Redis Streams** is the documented scale-out option; **Kafka is
  deferred** (operational weight not yet justified).
- **Message / idempotency.** Each approval carries an idempotency key
  (`request_id`); at-least-once delivery is de-duplicated by the single writer
  (proven below).
- **Per-company DB placement.** The eight per-company graphify DBs live in the
  **central shared CP behind the queue** (single-writer preserved), **not**
  replicated per workstation — replication would create multi-writer divergence,
  the exact ADR-0008 hazard. Workstation CPs hold only local working state;
  cross-company reads / writes route to the central CP.
- **Audit partitioning.** Per-tenant audit partitions; **tenant-0 (Kanbrick)**
  holds cross-company **aggregation** rights (firm-wide L5).
- **`expire_due` / budget sweeps.** Run on the **central writer** (sweep model,
  like `ScopeGrants::expire_due`), not per workstation, to avoid split-brain on
  shared budgets.

## Probe evidence

The throwaway spike `probes/approval-queue` (std only; built + tested here on
stable; **3 tests pass**) models the central queue as an MPSC channel feeding a
**single consumer / writer thread** that stamps a monotonic sequence (total order)
and does check-then-write against a ledger it **exclusively owns**:

- `two_concurrent_approvals_serialize_no_lost_update` — two threads race for a
  one-slot budget → exactly one `Approved`, one `RejectedOverBudget`, both durably
  recorded, the ledger debited once (never negative).
- `many_producers_single_total_order` — N producers → a contiguous `0..N` order;
  debits never exceed the starting budget.
- `duplicate_request_id_is_idempotent` — a re-delivered request is applied at most
  once.

This is ADR-0008's single-writer invariant extended to a multi-tenant central
queue: concurrency is safe because only the writer touches the ledger.

## Alternatives considered

- **Multi-writer, per-company DBs replicated per workstation.** Rejected: ADR-0008
  divergence / lost updates.
- **Kafka now.** Rejected: operational weight; Redis Streams is the lighter
  scale-out path if the CP-hosted queue is outgrown.
- **Per-workstation budget sweeps.** Rejected: split-brain on shared budgets;
  sweeps run centrally.

## Consequences

- Unblocks P14.2–14.6 (CompanyState / routing / catalogs) and P12.3 (budget
  central-queue).
- The central CP is a single point of failure for cross-tenant writes — size it
  for the approval fan-in; HA is a later ADR, mirroring ADR-0008.
- Single-writer is preserved end-to-end; the spike is the reproducible evidence.
