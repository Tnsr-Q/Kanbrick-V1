//! Graph schema vocabulary shared by the store and discovery layers.
//!
//! The concrete node structs (`PersonNode`, `CompanyNode`, ...) land in
//! `kanbrick-store` during Phase 1; this module fixes the label vocabulary so
//! every layer agrees on the same names.

use serde::{Deserialize, Serialize};

/// Node labels in the firm graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeLabel {
    /// An individual in the firm's org chart.
    Person,
    /// A portfolio company.
    Company,
    /// A business segment grouping companies.
    Segment,
}

/// Edge labels in the firm graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeLabel {
    /// `Person -[:ReportsTo]-> Person` — org-chart hierarchy.
    ReportsTo,
    /// `Person -[:Manages]-> Company` — management responsibility.
    Manages,
    /// `Company -[:BelongsToSegment]-> Segment` — segment assignment.
    BelongsToSegment,
}

impl NodeLabel {
    /// The label as it appears in Cypher (e.g. `Person`).
    pub fn as_cypher(self) -> &'static str {
        match self {
            NodeLabel::Person => "Person",
            NodeLabel::Company => "Company",
            NodeLabel::Segment => "Segment",
        }
    }
}

impl EdgeLabel {
    /// The relationship type as it appears in Cypher (e.g. `REPORTS_TO`).
    pub fn as_cypher(self) -> &'static str {
        match self {
            EdgeLabel::ReportsTo => "REPORTS_TO",
            EdgeLabel::Manages => "MANAGES",
            EdgeLabel::BelongsToSegment => "BELONGS_TO_SEGMENT",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cypher_names() {
        assert_eq!(NodeLabel::Person.as_cypher(), "Person");
        assert_eq!(EdgeLabel::ReportsTo.as_cypher(), "REPORTS_TO");
        assert_eq!(
            EdgeLabel::BelongsToSegment.as_cypher(),
            "BELONGS_TO_SEGMENT"
        );
    }
}
