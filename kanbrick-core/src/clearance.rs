//! The firm's five-tier clearance model.

use serde::{Deserialize, Serialize};

/// Access-clearance tiers, ordered from least (`L1`) to most (`L5`) privileged.
///
/// The derived [`Ord`]/[`PartialOrd`] implementations follow declaration order,
/// so `L1 < L2 < L3 < L4 < L5`. This lets gates express "at least level X"
/// checks directly, e.g. `ctx.clearance >= ClearanceLevel::L3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ClearanceLevel {
    /// L1 — Support. Own data and public company info only.
    L1,
    /// L2 — Execution. Assigned companies and own data.
    L2,
    /// L3 — Operational. Own segment's companies and direct reports.
    L3,
    /// L4 — Strategic. All companies and all persons.
    L4,
    /// L5 — Admin. Sees everything, unfiltered.
    L5,
}

impl ClearanceLevel {
    /// All clearance levels, ascending.
    pub const ALL: [ClearanceLevel; 5] = [
        ClearanceLevel::L1,
        ClearanceLevel::L2,
        ClearanceLevel::L3,
        ClearanceLevel::L4,
        ClearanceLevel::L5,
    ];

    /// Numeric rank in `1..=5`.
    pub fn rank(self) -> u8 {
        match self {
            ClearanceLevel::L1 => 1,
            ClearanceLevel::L2 => 2,
            ClearanceLevel::L3 => 3,
            ClearanceLevel::L4 => 4,
            ClearanceLevel::L5 => 5,
        }
    }

    /// Returns `true` if this clearance satisfies a required minimum level.
    pub fn satisfies(self, required: ClearanceLevel) -> bool {
        self >= required
    }
}

impl std::fmt::Display for ClearanceLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "L{}", self.rank())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_ascending() {
        assert!(ClearanceLevel::L1 < ClearanceLevel::L5);
        assert!(ClearanceLevel::L3 < ClearanceLevel::L4);
        assert_eq!(ClearanceLevel::ALL.len(), 5);
    }

    #[test]
    fn satisfies_minimum() {
        assert!(ClearanceLevel::L4.satisfies(ClearanceLevel::L3));
        assert!(ClearanceLevel::L3.satisfies(ClearanceLevel::L3));
        assert!(!ClearanceLevel::L2.satisfies(ClearanceLevel::L3));
    }

    #[test]
    fn rank_and_display() {
        assert_eq!(ClearanceLevel::L2.rank(), 2);
        assert_eq!(ClearanceLevel::L5.to_string(), "L5");
    }
}
