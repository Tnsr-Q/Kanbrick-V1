//! # kanbrick-guest-compliance
//!
//! WASM business guest: compliance checks over the firm graph (issues #41, #42).
//!
//! Two check families, returned as a single [`ComplianceReport`]:
//!
//! * **Org-chart integrity** (#41): every non-CEO has exactly one `REPORTS_TO`,
//!   there are no reporting cycles, and every company has a segment.
//! * **Clearance consistency** (#42): a person holding a `MANAGES` edge must have
//!   at least operational clearance ([`MANAGEMENT_MIN_CLEARANCE`]), every email is
//!   well-formed, and every person carries a valid clearance level.
//!
//! Running the guest requires **L4+** ([`REQUIRED_CLEARANCE`]) — it inspects the
//! whole graph, which only L4/L5 callers may see (the host filters `query_graph`
//! by clearance, so an L4+ caller is required for the checks to be meaningful).
//!
//! The pure check logic ([`run_checks`]) is unit-tested natively; the `wasm32`
//! entrypoint assembles [`OrgFacts`] from the graph via the SDK and runs it
//! (ADR-0004).

use std::collections::{HashMap, HashSet};

use kanbrick_core::ClearanceLevel;
use serde::{Deserialize, Serialize};

/// Minimum clearance a person must hold to legitimately manage a company.
pub const MANAGEMENT_MIN_CLEARANCE: ClearanceLevel = ClearanceLevel::L3;

/// Minimum clearance required to run the compliance guest at all.
pub const REQUIRED_CLEARANCE: ClearanceLevel = ClearanceLevel::L4;

/// Which check family (or both) to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComplianceCheckType {
    /// Org-chart integrity only (#41).
    OrgChart,
    /// Clearance consistency only (#42).
    Clearance,
    /// Both check families.
    All,
}

/// The kind of a compliance violation. Each kind is a distinct variant (#42).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationKind {
    /// A non-CEO person has no `REPORTS_TO` edge.
    MissingReportsTo,
    /// A person has more than one `REPORTS_TO` edge.
    MultipleManagers,
    /// The reporting graph contains a cycle.
    ReportingCycle,
    /// A company has no `BELONGS_TO_SEGMENT` edge.
    IncompleteSegmentAssignment,
    /// A person below [`MANAGEMENT_MIN_CLEARANCE`] holds a `MANAGES` edge.
    UnauthorizedManager,
    /// A person's email is malformed.
    InvalidEmail,
    /// A person has a missing or invalid clearance level.
    MissingClearance,
}

/// A single compliance finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Violation {
    /// What kind of violation this is.
    pub kind: ViolationKind,
    /// The offending entity (email or company code).
    pub subject: String,
    /// Human-readable detail.
    pub detail: String,
}

/// The result of a compliance run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComplianceReport {
    /// `true` when no violations were found.
    pub passed: bool,
    /// Which check families ran.
    pub checks_run: Vec<String>,
    /// Every violation found.
    pub violations: Vec<Violation>,
}

/// Facts about one person, assembled from the graph.
#[derive(Debug, Clone)]
pub struct PersonFacts {
    /// The person's email handle.
    pub email: String,
    /// The person's coarse role (e.g. `"CEO"`).
    pub role: String,
    /// Parsed clearance level; `None` when missing or unparseable.
    pub clearance: Option<ClearanceLevel>,
}

/// Everything the checks operate on — assembled from the graph (or a fixture).
#[derive(Debug, Clone, Default)]
pub struct OrgFacts {
    /// All persons.
    pub persons: Vec<PersonFacts>,
    /// Each person's managers (email → manager emails). Absent ⇒ no manager.
    pub manager_of: HashMap<String, Vec<String>>,
    /// All company codes.
    pub companies: Vec<String>,
    /// company code → segment code, for companies with an assignment.
    pub company_segment: HashMap<String, String>,
    /// Emails of persons holding at least one `MANAGES` edge.
    pub managers: HashSet<String>,
}

/// Parse a clearance string (`"L1"`..`"L5"`) into a [`ClearanceLevel`].
pub fn parse_clearance(raw: &str) -> Option<ClearanceLevel> {
    match raw {
        "L1" => Some(ClearanceLevel::L1),
        "L2" => Some(ClearanceLevel::L2),
        "L3" => Some(ClearanceLevel::L3),
        "L4" => Some(ClearanceLevel::L4),
        "L5" => Some(ClearanceLevel::L5),
        _ => None,
    }
}

/// A minimally well-formed email: exactly one `@`, a non-empty local part, and a
/// dotted domain.
pub fn is_valid_email(email: &str) -> bool {
    let mut parts = email.split('@');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(local), Some(domain), None) => {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        }
        _ => false,
    }
}

/// Run the selected check family(ies) and assemble a [`ComplianceReport`].
pub fn run_checks(check: ComplianceCheckType, facts: &OrgFacts) -> ComplianceReport {
    let mut violations = Vec::new();
    let mut checks_run = Vec::new();
    if matches!(
        check,
        ComplianceCheckType::OrgChart | ComplianceCheckType::All
    ) {
        checks_run.push("org_chart".to_string());
        violations.extend(check_org_chart(facts));
    }
    if matches!(
        check,
        ComplianceCheckType::Clearance | ComplianceCheckType::All
    ) {
        checks_run.push("clearance".to_string());
        violations.extend(check_clearance(facts));
    }
    ComplianceReport {
        passed: violations.is_empty(),
        checks_run,
        violations,
    }
}

/// Org-chart integrity (#41): exactly-one-manager (non-CEO), no cycles, complete
/// segment assignments.
pub fn check_org_chart(facts: &OrgFacts) -> Vec<Violation> {
    let mut violations = Vec::new();

    for person in &facts.persons {
        if person.role.eq_ignore_ascii_case("CEO") {
            continue; // the CEO is the single root and has no manager by design
        }
        let managers = facts
            .manager_of
            .get(&person.email)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        match managers.len() {
            0 => violations.push(Violation {
                kind: ViolationKind::MissingReportsTo,
                subject: person.email.clone(),
                detail: "non-CEO person has no REPORTS_TO edge".to_string(),
            }),
            1 => {}
            n => violations.push(Violation {
                kind: ViolationKind::MultipleManagers,
                subject: person.email.clone(),
                detail: format!("has {n} managers, expected exactly 1"),
            }),
        }
    }

    violations.extend(detect_cycles(&facts.manager_of));

    for company in &facts.companies {
        if !facts.company_segment.contains_key(company) {
            violations.push(Violation {
                kind: ViolationKind::IncompleteSegmentAssignment,
                subject: company.clone(),
                detail: "company has no BELONGS_TO_SEGMENT edge".to_string(),
            });
        }
    }

    violations
}

/// Detect reporting cycles by walking the first-manager successor chain from each
/// person. A node revisited within one walk is a cycle.
fn detect_cycles(manager_of: &HashMap<String, Vec<String>>) -> Vec<Violation> {
    let mut out = Vec::new();
    let mut globally_seen: HashSet<String> = HashSet::new();
    let mut starts: Vec<&String> = manager_of.keys().collect();
    starts.sort(); // determinism

    for start in starts {
        if globally_seen.contains(start) {
            continue;
        }
        let mut walk_set: HashSet<String> = HashSet::new();
        let mut walk: Vec<String> = Vec::new();
        let mut current = Some(start.clone());
        while let Some(node) = current {
            if walk_set.contains(&node) {
                out.push(Violation {
                    kind: ViolationKind::ReportingCycle,
                    subject: node.clone(),
                    detail: "reporting relationship forms a cycle".to_string(),
                });
                break;
            }
            if globally_seen.contains(&node) {
                break; // joins an already-explored acyclic chain
            }
            walk_set.insert(node.clone());
            walk.push(node.clone());
            current = manager_of.get(&node).and_then(|m| m.first()).cloned();
        }
        globally_seen.extend(walk);
    }

    out.sort_by(|a, b| a.subject.cmp(&b.subject));
    out.dedup_by(|a, b| a.subject == b.subject);
    out
}

/// Clearance consistency (#42): management authority, email format, valid clearance.
pub fn check_clearance(facts: &OrgFacts) -> Vec<Violation> {
    let mut violations = Vec::new();

    for person in &facts.persons {
        if !is_valid_email(&person.email) {
            violations.push(Violation {
                kind: ViolationKind::InvalidEmail,
                subject: person.email.clone(),
                detail: "email is not well-formed".to_string(),
            });
        }
        match person.clearance {
            None => violations.push(Violation {
                kind: ViolationKind::MissingClearance,
                subject: person.email.clone(),
                detail: "person has a missing or invalid clearance level".to_string(),
            }),
            Some(clearance) => {
                if facts.managers.contains(&person.email) && clearance < MANAGEMENT_MIN_CLEARANCE {
                    violations.push(Violation {
                        kind: ViolationKind::UnauthorizedManager,
                        subject: person.email.clone(),
                        detail: format!(
                            "clearance {clearance} holds a MANAGES edge but management \
                             requires {MANAGEMENT_MIN_CLEARANCE}+"
                        ),
                    });
                }
            }
        }
    }

    violations
}

// ---------------------------------------------------------------------------
// WASM entrypoint (ADR-0004): assemble facts from the graph, run the checks.
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
mod entrypoint {
    use super::*;
    use kanbrick_guest_sdk as sdk;
    use sdk::{GraphQuery, GraphRows, GuestRequest, GuestResponse, LogLevel};

    /// Read a string column from a result row.
    fn col<'a>(row: &'a sdk::serde_json::Value, key: &str) -> Option<&'a str> {
        row.get(key).and_then(|v| v.as_str())
    }

    fn query(cypher: &str) -> sdk::Result<GraphRows> {
        sdk::query_graph(&GraphQuery::new(cypher))
    }

    /// Assemble [`OrgFacts`] from the firm graph through the host (L4+ ⇒ full graph).
    fn assemble_facts() -> sdk::Result<OrgFacts> {
        let mut facts = OrgFacts::default();

        for row in query("MATCH (p:Person) RETURN p.email, p.role, p.clearance_level")?.rows {
            let email = col(&row, "email").unwrap_or_default().to_string();
            facts.persons.push(PersonFacts {
                role: col(&row, "role").unwrap_or_default().to_string(),
                clearance: col(&row, "clearance_level").and_then(parse_clearance),
                email,
            });
        }

        // REPORTS_TO is read per person: a two-`email`-column edge query would
        // collide under the store's prefix-stripping row mapper (ADR-0001).
        for person in &facts.persons {
            let rows = sdk::query_graph(
                &GraphQuery::new(
                    "MATCH (p:Person {email: $e})-[:REPORTS_TO]->(m:Person) RETURN m.email",
                )
                .param("e", person.email.clone()),
            )?;
            let managers: Vec<String> = rows
                .rows
                .iter()
                .filter_map(|r| col(r, "email").map(String::from))
                .collect();
            if !managers.is_empty() {
                facts.manager_of.insert(person.email.clone(), managers);
            }
        }

        for row in
            query("MATCH (p:Person)-[:MANAGES]->(c:Company) RETURN p.email, c.company_id")?.rows
        {
            if let Some(email) = col(&row, "email") {
                facts.managers.insert(email.to_string());
            }
        }

        for row in query("MATCH (c:Company) RETURN c.company_id")?.rows {
            if let Some(id) = col(&row, "company_id") {
                facts.companies.push(id.to_string());
            }
        }

        for row in query(
            "MATCH (c:Company)-[:BELONGS_TO_SEGMENT]->(s:Segment) RETURN c.company_id, s.code",
        )?
        .rows
        {
            if let (Some(c), Some(s)) = (col(&row, "company_id"), col(&row, "code")) {
                facts.company_segment.insert(c.to_string(), s.to_string());
            }
        }

        Ok(facts)
    }

    fn handle(request: GuestRequest) -> sdk::Result<GuestResponse> {
        sdk::log(LogLevel::Info, "compliance: started");
        let ctx = sdk::firm_context()?;
        if ctx.clearance < REQUIRED_CLEARANCE {
            return Err(sdk::Error::AccessDenied {
                required: REQUIRED_CLEARANCE,
                actual: ctx.clearance,
            });
        }

        let check = request
            .payload
            .get("check")
            .and_then(|v| sdk::serde_json::from_value(v.clone()).ok())
            .unwrap_or(ComplianceCheckType::All);

        let facts = assemble_facts()?;
        let report = run_checks(check, &facts);

        sdk::serde_json::to_value(&report)
            .map(GuestResponse::new)
            .map_err(|e| sdk::Error::Internal(format!("encoding report: {e}")))
    }

    sdk::guest_entrypoint!(handle);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Facts mirroring the firm seed — the compliant baseline.
    fn seed_facts() -> OrgFacts {
        let people = [
            ("tracy.brittcool@kanbrick.com", "CEO", "L5"),
            ("brian.humphrey@kanbrick.com", "President", "L5"),
            ("matt.berns@kanbrick.com", "CTO", "L4"),
            ("andrea.lewis@kanbrick.com", "CFO", "L4"),
            ("marcus.hall@kanbrick.com", "CPO", "L4"),
            ("peter.nash@kanbrick.com", "CSO", "L4"),
            ("tyler.begemann@kanbrick.com", "Segment Lead", "L3"),
            ("blake.richardson@kanbrick.com", "Segment Lead", "L3"),
            ("sloan.allen@kanbrick.com", "Segment Lead", "L3"),
            ("samantha.jordan@kanbrick.com", "Senior Analyst", "L2"),
            ("elena.ruiz@kanbrick.com", "Analyst", "L2"),
            ("dana.prescott@kanbrick.com", "Support Coordinator", "L1"),
        ];
        let persons = people
            .iter()
            .map(|(email, role, clr)| PersonFacts {
                email: (*email).to_string(),
                role: (*role).to_string(),
                clearance: parse_clearance(clr),
            })
            .collect();

        let reports = [
            (
                "brian.humphrey@kanbrick.com",
                "tracy.brittcool@kanbrick.com",
            ),
            ("dana.prescott@kanbrick.com", "tracy.brittcool@kanbrick.com"),
            ("matt.berns@kanbrick.com", "brian.humphrey@kanbrick.com"),
            ("andrea.lewis@kanbrick.com", "brian.humphrey@kanbrick.com"),
            ("marcus.hall@kanbrick.com", "brian.humphrey@kanbrick.com"),
            ("peter.nash@kanbrick.com", "brian.humphrey@kanbrick.com"),
            ("tyler.begemann@kanbrick.com", "peter.nash@kanbrick.com"),
            ("blake.richardson@kanbrick.com", "peter.nash@kanbrick.com"),
            ("sloan.allen@kanbrick.com", "peter.nash@kanbrick.com"),
            (
                "samantha.jordan@kanbrick.com",
                "tyler.begemann@kanbrick.com",
            ),
            ("elena.ruiz@kanbrick.com", "blake.richardson@kanbrick.com"),
        ];
        let mut manager_of: HashMap<String, Vec<String>> = HashMap::new();
        for (sub, mgr) in reports {
            manager_of.insert(sub.to_string(), vec![mgr.to_string()]);
        }

        let companies: Vec<String> = [
            "JMTS", "MCON", "AAG", "LTI", "ATS", "KEEP", "ASI", "DFPG", "BWK",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let segments = [
            ("JMTS", "TLS"),
            ("MCON", "TLS"),
            ("AAG", "TLS"),
            ("LTI", "TLS"),
            ("ATS", "TLS"),
            ("KEEP", "IND"),
            ("ASI", "IND"),
            ("DFPG", "MFG"),
            ("BWK", "STR"),
        ];
        let company_segment = segments
            .iter()
            .map(|(c, s)| (c.to_string(), s.to_string()))
            .collect();

        // MANAGES holders (all L3+ in the seed).
        let managers = [
            "tracy.brittcool@kanbrick.com",
            "brian.humphrey@kanbrick.com",
            "andrea.lewis@kanbrick.com",
            "matt.berns@kanbrick.com",
            "tyler.begemann@kanbrick.com",
            "blake.richardson@kanbrick.com",
            "sloan.allen@kanbrick.com",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        OrgFacts {
            persons,
            manager_of,
            companies,
            company_segment,
            managers,
        }
    }

    fn kinds(violations: &[Violation]) -> Vec<ViolationKind> {
        violations.iter().map(|v| v.kind).collect()
    }

    #[test]
    fn seed_data_has_no_violations() {
        let report = run_checks(ComplianceCheckType::All, &seed_facts());
        assert!(
            report.passed,
            "unexpected violations: {:?}",
            report.violations
        );
        assert!(report.violations.is_empty());
        assert_eq!(report.checks_run, vec!["org_chart", "clearance"]);
    }

    #[test]
    fn missing_reports_to_is_detected() {
        let mut facts = seed_facts();
        facts.manager_of.remove("elena.ruiz@kanbrick.com");
        let violations = check_org_chart(&facts);
        assert!(violations
            .iter()
            .any(|v| v.kind == ViolationKind::MissingReportsTo
                && v.subject == "elena.ruiz@kanbrick.com"));
    }

    #[test]
    fn reporting_cycle_is_detected() {
        let mut facts = seed_facts();
        // Make Peter report to Samantha: Peter→Samantha→Tyler→Peter is a cycle.
        facts.manager_of.insert(
            "peter.nash@kanbrick.com".to_string(),
            vec!["samantha.jordan@kanbrick.com".to_string()],
        );
        let violations = check_org_chart(&facts);
        assert!(kinds(&violations).contains(&ViolationKind::ReportingCycle));
    }

    #[test]
    fn incomplete_segment_assignment_is_detected() {
        let mut facts = seed_facts();
        facts.company_segment.remove("DFPG");
        let violations = check_org_chart(&facts);
        assert!(violations
            .iter()
            .any(|v| v.kind == ViolationKind::IncompleteSegmentAssignment && v.subject == "DFPG"));
    }

    #[test]
    fn l1_with_manages_edge_is_flagged() {
        // PRD checkpoint: an injected L1-with-MANAGES violation is detected.
        let mut facts = seed_facts();
        facts
            .managers
            .insert("dana.prescott@kanbrick.com".to_string()); // L1 now manages
        let violations = check_clearance(&facts);
        assert!(violations
            .iter()
            .any(|v| v.kind == ViolationKind::UnauthorizedManager
                && v.subject == "dana.prescott@kanbrick.com"));
    }

    #[test]
    fn invalid_email_is_flagged() {
        let mut facts = seed_facts();
        facts.persons[0].email = "not-an-email".to_string();
        let violations = check_clearance(&facts);
        assert!(kinds(&violations).contains(&ViolationKind::InvalidEmail));
        assert!(is_valid_email("a@b.com"));
        assert!(!is_valid_email("a@@b.com"));
        assert!(!is_valid_email("a@bcom"));
        assert!(!is_valid_email("@b.com"));
    }

    #[test]
    fn missing_clearance_is_flagged() {
        let mut facts = seed_facts();
        facts.persons[5].clearance = None;
        let violations = check_clearance(&facts);
        assert!(kinds(&violations).contains(&ViolationKind::MissingClearance));
    }

    #[test]
    fn each_violation_kind_is_distinct() {
        // A guard for "each violation kind has its own type variant" (#42).
        let all = [
            ViolationKind::MissingReportsTo,
            ViolationKind::MultipleManagers,
            ViolationKind::ReportingCycle,
            ViolationKind::IncompleteSegmentAssignment,
            ViolationKind::UnauthorizedManager,
            ViolationKind::InvalidEmail,
            ViolationKind::MissingClearance,
        ];
        let unique: HashSet<_> = all.iter().collect();
        assert_eq!(unique.len(), all.len());
    }

    #[test]
    fn check_type_selects_families() {
        let facts = seed_facts();
        assert_eq!(
            run_checks(ComplianceCheckType::OrgChart, &facts).checks_run,
            vec!["org_chart"]
        );
        assert_eq!(
            run_checks(ComplianceCheckType::Clearance, &facts).checks_run,
            vec!["clearance"]
        );
    }
}
