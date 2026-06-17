//! # kanbrick-guest-reporting
//!
//! WASM business guest: the clearance-tiered portfolio dashboard (issues #43, #44).
//!
//! The dashboard is built on the `PUBLIC_DATA` model (ADR-0005): the company
//! **roster** (id/name/segment) is public to every tier, while company **detail**
//! (stakeholders, management team) and **personnel** are clearance-gated by the
//! host's `query_graph` filter. The same report therefore yields tier-appropriate
//! output:
//!
//! | Tier | Roster | Company detail | Headcount |
//! | --- | --- | --- | --- |
//! | L5 / L4 | all 9 | all 9 | everyone |
//! | L3 | all 9 | own segment | self + reports |
//! | L2 | all 9 | assigned | self |
//! | L1 | all 9 | none | self |
//!
//! Every tier can *run* the report; the clearance shapes the result. The pure
//! aggregation ([`build_dashboard`]) is unit-tested natively; the `wasm32`
//! entrypoint assembles its input from the graph via the SDK (ADR-0004).

use std::collections::{BTreeMap, BTreeSet};

use kanbrick_core::ClearanceLevel;
use serde::{Deserialize, Serialize};

/// The report kinds this guest can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportType {
    /// The portfolio dashboard (#43/#44).
    #[default]
    PortfolioDashboard,
}

/// A public roster entry (company identity), visible to every clearance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterEntry {
    /// Company code.
    pub company_id: String,
    /// Trading name.
    pub name: String,
    /// Owning segment name.
    pub segment: String,
}

/// A management edge the caller is permitted to see (person manages company).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagementEdge {
    /// Managing person's email.
    pub person_email: String,
    /// Managed company code.
    pub company_id: String,
    /// Management scope (e.g. `segment_lead`).
    pub scope: String,
}

/// One member of a company's (visible) management team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagementMember {
    /// The person's email.
    pub email: String,
    /// Their management scope.
    pub scope: String,
}

/// Clearance-gated detail for a company the caller may see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanyDetail {
    /// Number of *visible* stakeholders.
    pub stakeholder_count: usize,
    /// The visible management team.
    pub management_team: Vec<ManagementMember>,
}

/// A dashboard company entry: public roster identity plus optional gated detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanyEntry {
    /// Company code (public).
    pub company_id: String,
    /// Trading name (public).
    pub name: String,
    /// Owning segment (public).
    pub segment: String,
    /// Detail, present only when the caller may see the company.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<CompanyDetail>,
}

/// A per-segment roll-up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentRollup {
    /// Segment name.
    pub segment: String,
    /// Companies in the segment (public roster count).
    pub company_count: usize,
    /// Distinct visible persons managing a company in the segment.
    pub person_count: usize,
}

/// Org-wide totals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgTotals {
    /// Total companies (public roster).
    pub company_count: usize,
    /// Distinct segments (public roster).
    pub segment_count: usize,
    /// Persons the caller can see.
    pub headcount: usize,
}

/// The assembled portfolio dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortfolioDashboard {
    /// The viewer's email.
    pub generated_for: String,
    /// The viewer's clearance (shapes the detail).
    pub clearance: ClearanceLevel,
    /// Companies (public roster, with gated detail where visible).
    pub companies: Vec<CompanyEntry>,
    /// Per-segment roll-ups.
    pub segments: Vec<SegmentRollup>,
    /// Org-wide totals.
    pub totals: OrgTotals,
}

/// Inputs to [`build_dashboard`], assembled from the (clearance-filtered) graph.
#[derive(Debug, Clone, Default)]
pub struct DashboardInput {
    /// The viewer's email.
    pub viewer_email: String,
    /// The viewer's clearance.
    pub clearance: Option<ClearanceLevel>,
    /// The public company roster (all companies).
    pub roster: Vec<RosterEntry>,
    /// Company codes the caller may see in detail (`can_see_company`).
    pub detail_visible: BTreeSet<String>,
    /// Management edges the caller may see.
    pub management: Vec<ManagementEdge>,
    /// Number of persons the caller may see (headcount).
    pub visible_headcount: usize,
}

/// Build the portfolio dashboard from clearance-filtered inputs (#43/#44).
pub fn build_dashboard(input: &DashboardInput) -> PortfolioDashboard {
    // company_id -> visible management edges.
    let mut mgmt_by_company: BTreeMap<&str, Vec<&ManagementEdge>> = BTreeMap::new();
    for edge in &input.management {
        mgmt_by_company
            .entry(edge.company_id.as_str())
            .or_default()
            .push(edge);
    }

    // company_id -> segment (from the public roster).
    let segment_of: BTreeMap<&str, &str> = input
        .roster
        .iter()
        .map(|c| (c.company_id.as_str(), c.segment.as_str()))
        .collect();

    let mut roster = input.roster.clone();
    roster.sort_by(|a, b| a.company_id.cmp(&b.company_id));

    let companies: Vec<CompanyEntry> = roster
        .iter()
        .map(|c| {
            let detail = if input.detail_visible.contains(&c.company_id) {
                let mut team: Vec<ManagementMember> = mgmt_by_company
                    .get(c.company_id.as_str())
                    .map(|edges| {
                        edges
                            .iter()
                            .map(|e| ManagementMember {
                                email: e.person_email.clone(),
                                scope: e.scope.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                team.sort_by(|a, b| a.email.cmp(&b.email));
                Some(CompanyDetail {
                    stakeholder_count: team.len(),
                    management_team: team,
                })
            } else {
                None
            };
            CompanyEntry {
                company_id: c.company_id.clone(),
                name: c.name.clone(),
                segment: c.segment.clone(),
                detail,
            }
        })
        .collect();

    // Per-segment roll-ups: company count from the roster, person count from the
    // distinct visible managers of companies in that segment.
    let mut companies_per_segment: BTreeMap<&str, usize> = BTreeMap::new();
    for c in &roster {
        *companies_per_segment.entry(c.segment.as_str()).or_default() += 1;
    }
    let mut persons_per_segment: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for edge in &input.management {
        if let Some(seg) = segment_of.get(edge.company_id.as_str()) {
            persons_per_segment
                .entry(seg)
                .or_default()
                .insert(edge.person_email.as_str());
        }
    }
    let segments: Vec<SegmentRollup> = companies_per_segment
        .iter()
        .map(|(seg, &company_count)| SegmentRollup {
            segment: (*seg).to_string(),
            company_count,
            person_count: persons_per_segment.get(seg).map_or(0, BTreeSet::len),
        })
        .collect();

    let totals = OrgTotals {
        company_count: roster.len(),
        segment_count: companies_per_segment.len(),
        headcount: input.visible_headcount,
    };

    PortfolioDashboard {
        generated_for: input.viewer_email.clone(),
        clearance: input.clearance.unwrap_or(ClearanceLevel::L1),
        companies,
        segments,
        totals,
    }
}

// ---------------------------------------------------------------------------
// WASM entrypoint (ADR-0004): assemble inputs from the graph, build the report.
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
mod entrypoint {
    use super::*;
    use kanbrick_guest_sdk as sdk;
    use sdk::{GraphQuery, GraphRows, GuestRequest, GuestResponse, LogLevel};

    fn col<'a>(row: &'a sdk::serde_json::Value, key: &str) -> Option<&'a str> {
        row.get(key).and_then(|v| v.as_str())
    }

    fn query(cypher: &str) -> sdk::Result<GraphRows> {
        sdk::query_graph(&GraphQuery::new(cypher))
    }

    /// Assemble the dashboard input from the firm graph, relying on the host's
    /// clearance filtering: the roster query is public; the detail/management/
    /// person queries come back scoped to the caller (ADR-0005).
    fn assemble(viewer_email: String, clearance: ClearanceLevel) -> sdk::Result<DashboardInput> {
        let mut input = DashboardInput {
            viewer_email,
            clearance: Some(clearance),
            ..Default::default()
        };

        // Public roster (company identity) — visible to every clearance.
        for row in query("MATCH (c:Company) RETURN c.company_id, c.name, c.segment")?.rows {
            if let (Some(id), Some(name), Some(seg)) = (
                col(&row, "company_id"),
                col(&row, "name"),
                col(&row, "segment"),
            ) {
                input.roster.push(RosterEntry {
                    company_id: id.to_string(),
                    name: name.to_string(),
                    segment: seg.to_string(),
                });
            }
        }

        // Companies the caller may see in *detail* — a sensitive projection, so
        // the host gates it by `can_see_company`.
        for row in query("MATCH (c:Company) RETURN c.company_id, c.description")?.rows {
            if let Some(id) = col(&row, "company_id") {
                input.detail_visible.insert(id.to_string());
            }
        }

        // Management edges the caller may see (person-gated rows).
        for row in query(
            "MATCH (p:Person)-[m:MANAGES]->(c:Company) RETURN p.email, c.company_id, m.scope",
        )?
        .rows
        {
            if let (Some(email), Some(company_id)) = (col(&row, "email"), col(&row, "company_id")) {
                input.management.push(ManagementEdge {
                    person_email: email.to_string(),
                    company_id: company_id.to_string(),
                    scope: col(&row, "scope").unwrap_or("unknown").to_string(),
                });
            }
        }

        // Headcount = persons the caller may see.
        input.visible_headcount = query("MATCH (p:Person) RETURN p.email")?.rows.len();

        Ok(input)
    }

    fn handle(request: GuestRequest) -> sdk::Result<GuestResponse> {
        sdk::log(LogLevel::Info, "reporting: started");
        let ctx = sdk::firm_context()?;

        let _report = request
            .payload
            .get("report")
            .and_then(|v| sdk::serde_json::from_value::<ReportType>(v.clone()).ok())
            .unwrap_or_default();

        let input = assemble(ctx.email.clone(), ctx.clearance)?;
        let dashboard = build_dashboard(&input);

        sdk::serde_json::to_value(&dashboard)
            .map(GuestResponse::new)
            .map_err(|e| sdk::Error::Internal(format!("encoding dashboard: {e}")))
    }

    sdk::guest_entrypoint!(handle);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster() -> Vec<RosterEntry> {
        [
            ("JMTS", "JM Test Systems", "Testing & Lab Services"),
            ("MCON", "Marine Concepts", "Testing & Lab Services"),
            ("AAG", "Alchemy Analytical Group", "Testing & Lab Services"),
            ("LTI", "Laboratory Testing Inc.", "Testing & Lab Services"),
            ("ATS", "Assured Testing Services", "Testing & Lab Services"),
            ("KEEP", "Keep Supply", "Industrial Distribution"),
            (
                "ASI",
                "Alabama Scale & Instrument",
                "Industrial Distribution",
            ),
            ("DFPG", "Depatie Fluid Power Group", "Manufacturing"),
            ("BWK", "Build with Kanbrick", "Strategic Programs"),
        ]
        .iter()
        .map(|(id, name, seg)| RosterEntry {
            company_id: (*id).to_string(),
            name: (*name).to_string(),
            segment: (*seg).to_string(),
        })
        .collect()
    }

    fn edge(person: &str, company: &str, scope: &str) -> ManagementEdge {
        ManagementEdge {
            person_email: person.to_string(),
            company_id: company.to_string(),
            scope: scope.to_string(),
        }
    }

    /// JMTS's five stakeholders, as in the seed.
    fn jmts_management() -> Vec<ManagementEdge> {
        vec![
            edge(
                "tracy.brittcool@kanbrick.com",
                "JMTS",
                "executive_oversight",
            ),
            edge(
                "brian.humphrey@kanbrick.com",
                "JMTS",
                "operational_oversight",
            ),
            edge("andrea.lewis@kanbrick.com", "JMTS", "financial_oversight"),
            edge("matt.berns@kanbrick.com", "JMTS", "technology_oversight"),
            edge("tyler.begemann@kanbrick.com", "JMTS", "segment_lead"),
        ]
    }

    #[test]
    fn l5_dashboard_is_complete() {
        // L5 sees every company in detail; JMTS shows its 5 stakeholders.
        let input = DashboardInput {
            viewer_email: "tracy.brittcool@kanbrick.com".into(),
            clearance: Some(ClearanceLevel::L5),
            roster: roster(),
            detail_visible: roster().iter().map(|c| c.company_id.clone()).collect(),
            management: jmts_management(),
            visible_headcount: 12,
        };
        let dash = build_dashboard(&input);

        assert_eq!(dash.companies.len(), 9);
        assert!(dash.companies.iter().all(|c| c.detail.is_some()));
        let jmts = dash
            .companies
            .iter()
            .find(|c| c.company_id == "JMTS")
            .unwrap();
        assert_eq!(jmts.detail.as_ref().unwrap().stakeholder_count, 5);
        assert_eq!(jmts.name, "JM Test Systems");

        // Org totals.
        assert_eq!(dash.totals.company_count, 9);
        assert_eq!(dash.totals.segment_count, 4);
        assert_eq!(dash.totals.headcount, 12);

        // Segment roll-ups: Testing & Lab Services has 5 companies.
        let tls = dash
            .segments
            .iter()
            .find(|s| s.segment == "Testing & Lab Services")
            .unwrap();
        assert_eq!(tls.company_count, 5);
    }

    #[test]
    fn l3_sees_roster_for_all_detail_for_segment() {
        // Tyler (L3): roster public (9), detail only for his 5 TLS companies, and
        // among JMTS stakeholders he sees only himself.
        let input = DashboardInput {
            viewer_email: "tyler.begemann@kanbrick.com".into(),
            clearance: Some(ClearanceLevel::L3),
            roster: roster(),
            detail_visible: ["JMTS", "MCON", "AAG", "LTI", "ATS"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            management: vec![edge("tyler.begemann@kanbrick.com", "JMTS", "segment_lead")],
            visible_headcount: 2, // self + Samantha
        };
        let dash = build_dashboard(&input);

        assert_eq!(dash.companies.len(), 9, "roster is public to all");
        let with_detail = dash.companies.iter().filter(|c| c.detail.is_some()).count();
        assert_eq!(with_detail, 5, "detail only for own-segment companies");

        let jmts = dash
            .companies
            .iter()
            .find(|c| c.company_id == "JMTS")
            .unwrap();
        let detail = jmts.detail.as_ref().unwrap();
        assert_eq!(detail.stakeholder_count, 1);
        assert_eq!(
            detail.management_team[0].email,
            "tyler.begemann@kanbrick.com"
        );

        // A company outside his segment is roster-only.
        let keep = dash
            .companies
            .iter()
            .find(|c| c.company_id == "KEEP")
            .unwrap();
        assert!(keep.detail.is_none());
        assert_eq!(keep.name, "Keep Supply"); // name still public
    }

    #[test]
    fn l1_sees_names_only() {
        // Dana (L1): full public roster, zero detail, headcount = just themselves.
        let input = DashboardInput {
            viewer_email: "dana.prescott@kanbrick.com".into(),
            clearance: Some(ClearanceLevel::L1),
            roster: roster(),
            detail_visible: BTreeSet::new(),
            management: Vec::new(),
            visible_headcount: 1,
        };
        let dash = build_dashboard(&input);

        assert_eq!(dash.companies.len(), 9);
        assert!(
            dash.companies.iter().all(|c| c.detail.is_none()),
            "L1 sees no company detail"
        );
        assert!(dash.companies.iter().all(|c| !c.name.is_empty()));
        assert_eq!(dash.totals.company_count, 9); // public
        assert_eq!(dash.totals.headcount, 1);
    }

    #[test]
    fn different_clearances_yield_different_output() {
        let l5 = build_dashboard(&DashboardInput {
            viewer_email: "tracy.brittcool@kanbrick.com".into(),
            clearance: Some(ClearanceLevel::L5),
            roster: roster(),
            detail_visible: roster().iter().map(|c| c.company_id.clone()).collect(),
            management: jmts_management(),
            visible_headcount: 12,
        });
        let l1 = build_dashboard(&DashboardInput {
            viewer_email: "dana.prescott@kanbrick.com".into(),
            clearance: Some(ClearanceLevel::L1),
            roster: roster(),
            detail_visible: BTreeSet::new(),
            management: Vec::new(),
            visible_headcount: 1,
        });
        let l5_detail = l5.companies.iter().filter(|c| c.detail.is_some()).count();
        let l1_detail = l1.companies.iter().filter(|c| c.detail.is_some()).count();
        assert_ne!(l5_detail, l1_detail);
        assert_ne!(l5.totals.headcount, l1.totals.headcount);
    }
}
