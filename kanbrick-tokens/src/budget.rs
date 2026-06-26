//! Budget primitives (P9.5, #105).
//!
//! A [`Budget`] is a per-employee (or per-project) spend ceiling in nano-USD, with
//! pure read-only checks against an already-known spend total (which the caller
//! reads from the [`TokenLedger`](crate::TokenLedger)). This crate **does not
//! enforce** budgets — it neither blocks a call nor debits anything. Enforcement is
//! P12.3 (ADR-0015): cross-user/budget approvals serialize through one central
//! queue, swept like `expire_due`, so debits never exceed the ceiling. These types
//! are the shared vocabulary that queue is built on.

use serde::{Deserialize, Serialize};

/// A spend ceiling in nano-USD (`1e-9` USD).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// The ceiling, in nano-USD.
    pub limit_nano_usd: u64,
}

/// Where a known spend total sits relative to a [`Budget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum BudgetStatus {
    /// Spend is at or under the ceiling; `remaining_nano_usd` is left.
    WithinBudget {
        /// Headroom left before the ceiling, in nano-USD.
        remaining_nano_usd: u64,
    },
    /// Spend has reached or passed the ceiling; `over_nano_usd` is the overage
    /// (`0` exactly at the ceiling).
    OverBudget {
        /// How far past the ceiling, in nano-USD.
        over_nano_usd: u64,
    },
}

impl Budget {
    /// A budget from a whole-USD ceiling (saturating).
    pub fn from_usd(usd: u64) -> Self {
        Budget {
            limit_nano_usd: usd.saturating_mul(1_000_000_000),
        }
    }

    /// A budget from an explicit nano-USD ceiling.
    pub fn from_nano(limit_nano_usd: u64) -> Self {
        Budget { limit_nano_usd }
    }

    /// Headroom remaining given `spent_nano_usd` already spent (saturating to `0`).
    pub fn remaining(&self, spent_nano_usd: u64) -> u64 {
        self.limit_nano_usd.saturating_sub(spent_nano_usd)
    }

    /// Whether spending `additional_nano_usd` on top of `spent_nano_usd` would
    /// cross the ceiling. The check P12.3 runs before approving a debit.
    pub fn would_exceed(&self, spent_nano_usd: u64, additional_nano_usd: u64) -> bool {
        spent_nano_usd.saturating_add(additional_nano_usd) > self.limit_nano_usd
    }

    /// Classify a known spend total against this budget.
    pub fn status(&self, spent_nano_usd: u64) -> BudgetStatus {
        if spent_nano_usd <= self.limit_nano_usd {
            BudgetStatus::WithinBudget {
                remaining_nano_usd: self.limit_nano_usd - spent_nano_usd,
            }
        } else {
            BudgetStatus::OverBudget {
                over_nano_usd: spent_nano_usd - self.limit_nano_usd,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_usd_converts_to_nano() {
        assert_eq!(Budget::from_usd(5).limit_nano_usd, 5_000_000_000);
    }

    #[test]
    fn remaining_and_would_exceed() {
        let b = Budget::from_nano(1_000);
        assert_eq!(b.remaining(400), 600);
        assert_eq!(b.remaining(2_000), 0); // saturates, never underflows
        assert!(!b.would_exceed(400, 600)); // 1000 == limit is not "exceed"
        assert!(b.would_exceed(400, 601)); // 1001 > 1000
    }

    #[test]
    fn status_classifies_within_at_and_over() {
        let b = Budget::from_nano(1_000);
        assert_eq!(
            b.status(400),
            BudgetStatus::WithinBudget {
                remaining_nano_usd: 600
            }
        );
        // Exactly at the ceiling is still within (zero remaining).
        assert_eq!(
            b.status(1_000),
            BudgetStatus::WithinBudget {
                remaining_nano_usd: 0
            }
        );
        assert_eq!(
            b.status(1_250),
            BudgetStatus::OverBudget { over_nano_usd: 250 }
        );
    }
}
