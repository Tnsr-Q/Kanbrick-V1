//! P8.7 (#99) — central approval queue that serializes concurrent cross-user
//! approvals through a **single writer**.
//!
//! Throwaway de-risk spike (see `docs/adr/0015-tenancy-topology.md`). The tenancy
//! decision is **per-workstation control plane (CP) + a central approval queue**:
//! each Cockpit runs its own `kanbrick-api` CP (its writes are local), but
//! cross-user scope/budget **approvals serialize through one shared queue**.
//!
//! The risk this spike retires: two leads (or two workstations) approving against
//! the **same** budget/scope concurrently could double-spend or lose an update,
//! violating ADR-0008's invariant that there is exactly one write point. The spike
//! models the queue as an MPSC channel feeding a **single consumer thread** — the
//! sole writer. Producers on many threads enqueue requests; the writer applies
//! them one at a time in arrival order, stamping each with a monotonic sequence
//! number (the total order) and doing a check-then-write against the ledger under
//! its exclusive ownership. Because only the writer touches the ledger, a
//! check-then-write is atomic with respect to other approvals — no lost update,
//! no double-spend.
//!
//! std only (`std::thread` + `std::sync::mpsc`), so it runs in any environment.

use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

/// An approval request entering the central queue. In production this is a
/// budget grant or a `ScopeGrants::approve` (kanbrick-discovery/src/grants.rs)
/// crossing from a workstation CP into the central writer.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// Idempotency key — a re-delivered request with the same id is applied once.
    pub request_id: String,
    /// The shared budget/scope this approval debits.
    pub account: String,
    /// Amount of budget to commit (e.g. token dollars).
    pub amount: u64,
    /// Who is approving (the grantor identity; audit + eligibility upstream).
    pub grantor: String,
}

/// The durable record the single writer produces for every request it processes
/// — both approvals and rejections are recorded (nothing is lost).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRecord {
    /// Total-order position assigned by the single writer (0-based, contiguous).
    pub seq: u64,
    pub request_id: String,
    pub account: String,
    pub amount: u64,
    pub grantor: String,
    pub outcome: Outcome,
    /// Ledger balance remaining on `account` *after* this request was applied.
    pub balance_after: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Approved,
    /// Refused because the account had insufficient remaining budget.
    RejectedOverBudget,
    /// A duplicate `request_id` already applied — idempotent no-op.
    DuplicateIgnored,
}

/// A handle producers use to submit approvals to the central queue.
#[derive(Clone)]
pub struct QueueHandle {
    tx: Sender<ApprovalRequest>,
}

impl QueueHandle {
    /// Enqueue an approval. Returns once the request is *queued* (not yet
    /// applied) — ordering is decided by the single writer on dequeue.
    pub fn submit(&self, req: ApprovalRequest) {
        // If the writer has hung up the channel is closed; a real CP would
        // surface that. For the spike, ignore the (post-shutdown) error.
        let _ = self.tx.send(req);
    }
}

/// The central writer: owns the ledger and is the *only* thing that mutates it.
pub struct CentralWriter {
    handle: JoinHandle<Vec<ApprovalRecord>>,
    tx: Option<Sender<ApprovalRequest>>,
}

impl CentralWriter {
    /// Start the single-writer thread seeded with `account -> starting_budget`.
    pub fn start(initial_budgets: Vec<(String, u64)>) -> Self {
        let (tx, rx) = mpsc::channel::<ApprovalRequest>();
        let handle = thread::spawn(move || {
            // ----- exclusively owned by this one thread: the single writer -----
            let mut balances: std::collections::HashMap<String, u64> =
                initial_budgets.into_iter().collect();
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut log: Vec<ApprovalRecord> = Vec::new();
            let mut seq: u64 = 0;

            // Process strictly in arrival order. `recv` blocks; the loop ends when
            // every producer handle is dropped (channel closed).
            while let Ok(req) = rx.recv() {
                let balance = balances.entry(req.account.clone()).or_insert(0);
                let outcome = if !seen.insert(req.request_id.clone()) {
                    Outcome::DuplicateIgnored
                } else if *balance >= req.amount {
                    // check-then-write is atomic here because no other thread can
                    // touch `balance` — this is the whole point of one writer.
                    *balance -= req.amount;
                    Outcome::Approved
                } else {
                    Outcome::RejectedOverBudget
                };
                log.push(ApprovalRecord {
                    seq,
                    request_id: req.request_id,
                    account: req.account,
                    amount: req.amount,
                    grantor: req.grantor,
                    outcome,
                    balance_after: *balance,
                });
                seq += 1;
            }
            log
        });
        CentralWriter {
            handle,
            tx: Some(tx),
        }
    }

    /// A cloneable producer handle for a workstation CP.
    pub fn handle(&self) -> QueueHandle {
        QueueHandle {
            tx: self.tx.clone().expect("writer still running"),
        }
    }

    /// Close the queue and drain the writer, returning the durable, totally
    /// ordered log of every request it processed.
    ///
    /// All outstanding [`QueueHandle`]s must be dropped first: an MPSC channel
    /// stays open while *any* producer handle lives, so the writer's `recv` loop
    /// would otherwise block forever. (In the concurrent tests each handle is
    /// moved into its producer thread and dropped when that thread exits.)
    pub fn shutdown(mut self) -> Vec<ApprovalRecord> {
        // Drop the writer's own sender so the channel closes once all producer
        // handles are gone, ending the `recv` loop.
        self.tx.take();
        self.handle.join().expect("writer thread panicked")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two concurrent approvals race for a budget that only covers ONE. Exactly
    /// one must win; both must be durably recorded; the ledger must show a single
    /// debit (no lost update / no double-spend).
    #[test]
    fn two_concurrent_approvals_serialize_no_lost_update() {
        let writer = CentralWriter::start(vec![("budget-A".into(), 100)]);

        // Two workstations, each approving 100 against the same 100-unit budget.
        let mut producers = Vec::new();
        for i in 0..2 {
            let h = writer.handle();
            producers.push(thread::spawn(move || {
                h.submit(ApprovalRequest {
                    request_id: format!("req-{i}"),
                    account: "budget-A".into(),
                    amount: 100,
                    grantor: format!("lead-{i}"),
                });
            }));
        }
        for p in producers {
            p.join().unwrap();
        }

        let log = writer.shutdown();

        // Both requests are durably recorded — nothing is lost.
        assert_eq!(log.len(), 2, "both approvals must be recorded");

        // Exactly one Approved, exactly one RejectedOverBudget.
        let approved = log
            .iter()
            .filter(|r| r.outcome == Outcome::Approved)
            .count();
        let rejected = log
            .iter()
            .filter(|r| r.outcome == Outcome::RejectedOverBudget)
            .count();
        assert_eq!(approved, 1, "exactly one approval may win the budget");
        assert_eq!(rejected, 1, "the loser is recorded as over-budget");

        // The ledger reflects a single debit: final balance is 0, never negative,
        // never double-spent below zero.
        let final_balance = log.iter().map(|r| r.balance_after).min().unwrap();
        assert_eq!(final_balance, 0, "exactly one debit applied");

        // The writer imposed a total order: contiguous, unique sequence numbers.
        let mut seqs: Vec<u64> = log.iter().map(|r| r.seq).collect();
        seqs.sort_unstable();
        assert_eq!(seqs, vec![0, 1]);
    }

    /// N producers, all approved within budget → N records, unique contiguous
    /// order, and the sum of debits never exceeds the starting budget.
    #[test]
    fn many_producers_single_total_order() {
        const N: u64 = 50;
        let writer = CentralWriter::start(vec![("budget-B".into(), N * 10)]);

        let mut producers = Vec::new();
        for i in 0..N {
            let h = writer.handle();
            producers.push(thread::spawn(move || {
                h.submit(ApprovalRequest {
                    request_id: format!("b-{i}"),
                    account: "budget-B".into(),
                    amount: 10,
                    grantor: "lead".into(),
                });
            }));
        }
        for p in producers {
            p.join().unwrap();
        }
        let log = writer.shutdown();

        assert_eq!(log.len() as u64, N);
        let approved: u64 = log
            .iter()
            .filter(|r| r.outcome == Outcome::Approved)
            .map(|r| r.amount)
            .sum();
        assert!(approved <= N * 10, "never debit beyond the starting budget");

        // Sequence numbers form exactly 0..N — a single total order.
        let mut seqs: Vec<u64> = log.iter().map(|r| r.seq).collect();
        seqs.sort_unstable();
        assert_eq!(seqs, (0..N).collect::<Vec<_>>());

        // Final balance is exactly start - sum(approved): no lost update.
        let final_balance = log.last().unwrap().balance_after;
        assert_eq!(final_balance, N * 10 - approved);
    }

    /// A re-delivered request (same `request_id`) is applied at most once —
    /// idempotency the central queue needs for at-least-once delivery.
    #[test]
    fn duplicate_request_id_is_idempotent() {
        let writer = CentralWriter::start(vec![("budget-C".into(), 100)]);
        let h = writer.handle();
        let req = ApprovalRequest {
            request_id: "same".into(),
            account: "budget-C".into(),
            amount: 30,
            grantor: "lead".into(),
        };
        h.submit(req.clone());
        h.submit(req);
        drop(h); // release the producer handle so the queue can close (see `shutdown`)
        let log = writer.shutdown();

        assert_eq!(log.len(), 2);
        assert_eq!(log[0].outcome, Outcome::Approved);
        assert_eq!(log[1].outcome, Outcome::DuplicateIgnored);
        // Debited once: 100 - 30 = 70.
        assert_eq!(log[1].balance_after, 70);
    }
}
