//! Firm graph schema: typed node structs and the schema DDL (issue #8).
//!
//! The label/edge *vocabulary* lives in [`kanbrick_core::schema`]; this module
//! adds the concrete row structs the store deserializes into, plus the Cypher
//! statements that establish uniqueness constraints and lookup indexes.
//!
//! ## Cypher dialect note (HITL — issue #8)
//!
//! SparrowDB supports the **legacy** schema DDL grammar:
//!
//! * `CREATE CONSTRAINT ON (n:Label) ASSERT n.prop IS UNIQUE`
//! * `CREATE INDEX ON :Label(prop)`
//!
//! It does **not** yet support the newer
//! `CREATE CONSTRAINT name IF NOT EXISTS FOR (n:Label) REQUIRE n.prop IS UNIQUE`
//! form (an explicit upstream gap). The DDL emitted here is therefore written in
//! the supported legacy form. Idempotency is provided by the migration runner
//! (each migration version applies once), not by `IF NOT EXISTS`.

use kanbrick_core::{ClearanceLevel, Status};
use serde::{Deserialize, Serialize};

/// A `Person` node — an individual in the firm's org chart.
///
/// Fields mirror the properties set in `seed/kanbrick_seed_data.cypher`. The
/// optional fields are present only on some persons (e.g. `segment` on segment
/// leads, `note` on placeholder records).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonNode {
    /// Full display name, e.g. `"Tracy Britt Cool"`.
    pub full_name: String,
    /// Given name.
    pub first_name: String,
    /// Family name.
    pub last_name: String,
    /// Email — the person's unique login handle.
    pub email: String,
    /// Job title, e.g. `"Chief Executive Officer"`.
    pub title: String,
    /// Coarse role, e.g. `"CEO"`, `"Segment Lead"`, `"Analyst"`.
    pub role: String,
    /// Access clearance tier (`L1`..`L5`).
    pub clearance_level: ClearanceLevel,
    /// Human-readable clearance label, e.g. `"Admin"`.
    pub clearance_label: String,
    /// Department, e.g. `"Executive"`.
    pub department: String,
    /// Lifecycle status.
    pub status: Status,
    /// Owning segment name, when the person is scoped to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment: Option<String>,
    /// Free-text provenance note, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A `Company` node — a portfolio company.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanyNode {
    /// Short unique code, e.g. `"JMTS"`.
    pub company_id: String,
    /// Trading name.
    pub name: String,
    /// Registered legal name.
    pub legal_name: String,
    /// Owning segment name.
    pub segment: String,
    /// Lifecycle status.
    pub status: Status,
    /// Year the firm acquired the company.
    pub acquired_year: i64,
    /// US state of the headquarters.
    pub hq_state: String,
    /// One-line description of the business.
    pub description: String,
}

/// A `Segment` node — a grouping of portfolio companies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentNode {
    /// Segment display name, e.g. `"Testing & Lab Services"`.
    pub name: String,
    /// Short code, e.g. `"TLS"`.
    pub code: String,
    /// One-line description.
    pub description: String,
}

/// Uniqueness constraints, in the SparrowDB-supported legacy syntax.
///
/// * `Person.email` — unique login handle.
/// * `Company.company_id` — unique company code.
/// * `Segment.name` — unique segment name.
pub const CONSTRAINT_STATEMENTS: &[&str] = &[
    "CREATE CONSTRAINT ON (p:Person) ASSERT p.email IS UNIQUE",
    "CREATE CONSTRAINT ON (c:Company) ASSERT c.company_id IS UNIQUE",
    "CREATE CONSTRAINT ON (s:Segment) ASSERT s.name IS UNIQUE",
];

/// Lookup indexes for frequently filtered properties.
pub const INDEX_STATEMENTS: &[&str] = &[
    "CREATE INDEX ON :Person(clearance_level)",
    "CREATE INDEX ON :Person(full_name)",
];

/// All schema DDL (constraints then indexes), applied by the initial migration.
pub fn schema_statements() -> Vec<&'static str> {
    CONSTRAINT_STATEMENTS
        .iter()
        .chain(INDEX_STATEMENTS.iter())
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn person_node_deserializes_seed_shape() {
        let json = serde_json::json!({
            "full_name": "Tracy Britt Cool",
            "first_name": "Tracy",
            "last_name": "Britt Cool",
            "email": "tracy.brittcool@kanbrick.com",
            "title": "Chief Executive Officer",
            "role": "CEO",
            "clearance_level": "L5",
            "clearance_label": "Admin",
            "department": "Executive",
            "status": "active"
        });
        let p: PersonNode = serde_json::from_value(json).unwrap();
        assert_eq!(p.clearance_level, ClearanceLevel::L5);
        assert_eq!(p.status, Status::Active);
        assert!(p.segment.is_none());
    }

    #[test]
    fn schema_statements_are_legacy_syntax() {
        let all = schema_statements();
        assert_eq!(all.len(), 5);
        assert!(all[0].contains("ASSERT") && all[0].contains("IS UNIQUE"));
        assert!(all.iter().any(|s| s.starts_with("CREATE INDEX ON :Person")));
    }
}
