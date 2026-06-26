# Probe — central approval queue, single-writer serialization (P8.7 / #99)

A **throwaway Phase-8 de-risk spike** for `docs/adr/0015-tenancy-topology.md`.

## What it proves

The tenancy decision is **per-workstation control plane (CP) + a central approval
queue**: each Cockpit runs its own `kanbrick-api` CP (local writes), but
cross-user scope/budget **approvals serialize through one shared queue**. The risk
this retires: two leads (or two workstations) approving against the **same**
budget/scope concurrently could double-spend or lose an update, violating
ADR-0008's single-writer invariant.

The spike models the queue as an MPSC channel feeding a **single consumer thread**
— the sole writer. Many producer threads enqueue; the writer applies them one at a
time in arrival order, stamping a monotonic sequence number (the total order) and
doing a check-then-write against the ledger it exclusively owns. Because only the
writer touches the ledger, check-then-write is atomic w.r.t. other approvals.

## Run

```bash
cd probes/approval-queue
cargo test    # concurrent race → exactly one winner, no lost update; total order; idempotent re-delivery
```

Tests:
- `two_concurrent_approvals_serialize_no_lost_update` — two threads race for a
  budget covering one; exactly one `Approved`, one `RejectedOverBudget`, both
  durably recorded, single debit (balance never goes negative).
- `many_producers_single_total_order` — N producers → contiguous `0..N` sequence,
  debits never exceed the starting budget.
- `duplicate_request_id_is_idempotent` — re-delivered request applied at most once
  (at-least-once delivery safety).

Dependency-free (std `thread` + `mpsc`); runs anywhere. Excluded from the root
workspace (its own `[workspace]`).
