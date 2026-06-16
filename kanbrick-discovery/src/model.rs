//! Firm-typed result structs returned by the discovery engine.
//!
//! Every discovery answer speaks the firm's vocabulary
//! ([`PersonId`]/[`CompanyId`]/[`SegmentCode`], emails, company codes) — never
//! graphify-rs internals — so downstream phases consume a stable surface
//! (ADR-0003).

use kanbrick_core::{ClearanceLevel, CompanyId, PersonId, SegmentCode};
use kanbrick_store::{CompanyNode, PersonNode};
use serde::{Deserialize, Serialize};

/// A lightweight reference to a person, suitable for embedding in results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonRef {
    /// The person's email handle.
    pub id: PersonId,
    /// Full display name.
    pub full_name: String,
    /// Coarse role, e.g. `"CEO"`, `"Segment Lead"`.
    pub role: String,
    /// Access clearance tier.
    pub clearance: ClearanceLevel,
}

impl PersonRef {
    /// Build a reference from a loaded [`PersonNode`].
    pub fn from_node(node: &PersonNode) -> Self {
        PersonRef {
            id: PersonId::from(node.email.as_str()),
            full_name: node.full_name.clone(),
            role: node.role.clone(),
            clearance: node.clearance_level,
        }
    }

    /// The person's email, as a string slice.
    pub fn email(&self) -> &str {
        self.id.as_str()
    }
}

/// A lightweight reference to a portfolio company.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanyRef {
    /// Short unique company code, e.g. `"JMTS"`.
    pub id: CompanyId,
    /// Trading name.
    pub name: String,
    /// Owning segment name.
    pub segment: String,
}

impl CompanyRef {
    /// Build a reference from a loaded [`CompanyNode`].
    pub fn from_node(node: &CompanyNode) -> Self {
        CompanyRef {
            id: CompanyId::from(node.company_id.as_str()),
            name: node.name.clone(),
            segment: node.segment.clone(),
        }
    }
}

/// The scope of a person's management responsibility over a company, taken from
/// the `MANAGES` edge's `scope` property.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", from = "String")]
pub enum ManageScope {
    /// CEO-level oversight of the whole portfolio.
    ExecutiveOversight,
    /// President-level operational oversight.
    OperationalOversight,
    /// CFO-level financial oversight.
    FinancialOversight,
    /// CTO-level technology oversight.
    TechnologyOversight,
    /// Segment lead for the company's segment.
    SegmentLead,
    /// Program lead (e.g. Build with Kanbrick).
    ProgramLead,
    /// Any other scope string present in the data.
    Other(String),
}

impl ManageScope {
    /// Classify a raw `scope` string from a `MANAGES` edge.
    pub fn from_raw(s: &str) -> Self {
        match s {
            "executive_oversight" => ManageScope::ExecutiveOversight,
            "operational_oversight" => ManageScope::OperationalOversight,
            "financial_oversight" => ManageScope::FinancialOversight,
            "technology_oversight" => ManageScope::TechnologyOversight,
            "segment_lead" => ManageScope::SegmentLead,
            "program_lead" => ManageScope::ProgramLead,
            other => ManageScope::Other(other.to_string()),
        }
    }

    /// The scope as the raw string it serializes to.
    pub fn as_str(&self) -> &str {
        match self {
            ManageScope::ExecutiveOversight => "executive_oversight",
            ManageScope::OperationalOversight => "operational_oversight",
            ManageScope::FinancialOversight => "financial_oversight",
            ManageScope::TechnologyOversight => "technology_oversight",
            ManageScope::SegmentLead => "segment_lead",
            ManageScope::ProgramLead => "program_lead",
            ManageScope::Other(s) => s,
        }
    }

    /// Whether this is the dedicated lead scope for a segment/program (as opposed
    /// to firm-wide executive/financial/technology oversight).
    pub fn is_lead(&self) -> bool {
        matches!(self, ManageScope::SegmentLead | ManageScope::ProgramLead)
    }
}

impl From<String> for ManageScope {
    fn from(s: String) -> Self {
        ManageScope::from_raw(&s)
    }
}

impl From<ManageScope> for String {
    fn from(scope: ManageScope) -> Self {
        match scope {
            ManageScope::Other(s) => s,
            other => other.as_str().to_string(),
        }
    }
}

/// A person who manages a company, with the scope of that management.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stakeholder {
    /// The managing person.
    pub person: PersonRef,
    /// The scope of their management.
    pub scope: ManageScope,
}

/// The shortest `REPORTS_TO` chain between two people.
///
/// `steps[0]` is the `from` person and `steps[last]` is the `to` person; each
/// adjacent pair is connected by a single `REPORTS_TO` edge (in either
/// direction, since the chain may go up and/or down the tree).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportingPath {
    /// The people on the path, in order from `from` to `to`.
    pub steps: Vec<PersonRef>,
}

impl ReportingPath {
    /// The number of hops (edges) on the path. A path of `n` people has `n-1`
    /// hops; an empty path has length 0.
    pub fn len(&self) -> usize {
        self.steps.len().saturating_sub(1)
    }

    /// Whether the path contains no people.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// Direct and indirect report counts for a person.
///
/// Per issue #32's acceptance ("the CEO's indirect-report count equals total
/// employees minus one"), `indirect_reports` counts **every** subordinate
/// beneath the person transitively (the whole subtree below them), of which
/// `direct_reports` is the immediate subset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanMetrics {
    /// The subject person.
    pub person: PersonId,
    /// Number of immediate reports.
    pub direct_reports: usize,
    /// Number of subordinates anywhere beneath the person (transitive).
    pub indirect_reports: usize,
    /// Depth of the deepest reporting chain below the person (0 for a leaf).
    pub max_depth: usize,
}

/// Everyone within N reporting hops of a person, with the reporting edges among
/// them — the `org_neighborhood` sub-graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgNeighborhood {
    /// The person at the centre.
    pub center: PersonId,
    /// The hop radius requested.
    pub depth: usize,
    /// Members within `depth` hops (includes the centre at depth 0).
    pub members: Vec<PersonRef>,
    /// `REPORTS_TO` edges (subordinate, manager) among the members.
    pub reporting_edges: Vec<(PersonId, PersonId)>,
}

/// A summary of a business segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentReport {
    /// Segment code, e.g. `"TLS"`.
    pub code: SegmentCode,
    /// Segment display name.
    pub name: String,
    /// Companies in the segment.
    pub companies: Vec<CompanyRef>,
    /// People relevant to the segment (its lead plus stakeholders).
    pub persons: Vec<PersonRef>,
    /// The segment/program lead, if one is identifiable.
    pub lead: Option<PersonRef>,
}

/// A person who manages companies spanning more than one segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossSegmentLink {
    /// The person.
    pub person: PersonRef,
    /// The distinct segment codes they touch (sorted).
    pub segments: Vec<SegmentCode>,
}

/// A structural-importance ranking entry (graphify pagerank / degree).
///
/// Importance is **undirected** structural centrality over the firm graph
/// (ADR-0003) — it answers "who/what is most connected", not reporting rank.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InfluenceRank {
    /// The firm id of the entity (email / company code / segment code).
    pub id: String,
    /// The entity's display label.
    pub label: String,
    /// The kind of firm entity.
    pub kind: kanbrick_core::NodeLabel,
    /// The importance score (higher = more central).
    pub score: f64,
}
