//! Portfolio analytics over `MANAGES` / `BELONGS_TO_SEGMENT` (issues #33, #34).

use std::collections::{BTreeMap, BTreeSet};

use kanbrick_core::{Error, Result, SegmentCode};

use crate::graph::DiscoveryGraph;
use crate::model::{CompanyRef, CrossSegmentLink, PersonRef, SegmentReport, Stakeholder};
use crate::DiscoveryEngine;

/// Build a [`PersonRef`] for a known-loaded email.
fn person_ref(graph: &DiscoveryGraph, email: &str) -> Option<PersonRef> {
    graph.person(email).map(PersonRef::from_node)
}

impl DiscoveryEngine {
    /// Every person who manages `company`, with the scope of their management.
    ///
    /// A non-existent company yields an empty result. (Issue #33.)
    pub fn company_stakeholders(&self, company: &str) -> Result<Vec<Stakeholder>> {
        let graph = &self.graph;
        let Some(company) = graph.resolve_company(company) else {
            return Ok(Vec::new());
        };
        let company_id = company.company_id.clone();

        let mut stakeholders: Vec<Stakeholder> = graph
            .manages()
            .iter()
            .filter(|m| m.company_id == company_id)
            .filter_map(|m| {
                person_ref(graph, &m.person_email).map(|person| Stakeholder {
                    person,
                    scope: m.scope.clone(),
                })
            })
            .collect();
        stakeholders.sort_by(|a, b| a.person.id.cmp(&b.person.id));
        Ok(stakeholders)
    }

    /// A summary of a segment: its companies, the relevant persons, and the
    /// segment/program lead. Unknown segments are [`Error::NotFound`].
    /// (Issue #34.)
    pub fn segment_overview(&self, segment: &str) -> Result<SegmentReport> {
        let graph = &self.graph;
        let segment = graph
            .resolve_segment(segment)
            .ok_or_else(|| Error::NotFound(format!("segment {segment}")))?;
        let code = segment.code.clone();

        // Companies in the segment.
        let mut companies: Vec<CompanyRef> = graph
            .companies()
            .filter(|c| graph.segment_of_company(&c.company_id) == Some(code.as_str()))
            .map(CompanyRef::from_node)
            .collect();
        companies.sort_by(|a, b| a.id.cmp(&b.id));
        let company_ids: BTreeSet<&str> = companies.iter().map(|c| c.id.as_str()).collect();

        // Persons managing any company in the segment, and the dedicated lead.
        let mut persons: BTreeMap<String, PersonRef> = BTreeMap::new();
        let mut lead: Option<PersonRef> = None;
        for m in graph.manages() {
            if !company_ids.contains(m.company_id.as_str()) {
                continue;
            }
            if let Some(pr) = person_ref(graph, &m.person_email) {
                if m.scope.is_lead() && lead.is_none() {
                    lead = Some(pr.clone());
                }
                persons.insert(m.person_email.clone(), pr);
            }
        }

        Ok(SegmentReport {
            code: SegmentCode::from(code.as_str()),
            name: segment.name.clone(),
            companies,
            persons: persons.into_values().collect(),
            lead,
        })
    }

    /// People who manage companies spanning more than one segment. (Issue #34.)
    pub fn cross_segment_links(&self) -> Result<Vec<CrossSegmentLink>> {
        let graph = &self.graph;

        // person email → set of segment codes they touch.
        let mut by_person: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for m in graph.manages() {
            if let Some(seg) = graph.segment_of_company(&m.company_id) {
                by_person
                    .entry(m.person_email.clone())
                    .or_default()
                    .insert(seg.to_string());
            }
        }

        let mut links = Vec::new();
        for (email, segments) in by_person {
            if segments.len() > 1 {
                if let Some(person) = person_ref(graph, &email) {
                    links.push(CrossSegmentLink {
                        person,
                        segments: segments.into_iter().map(SegmentCode::from).collect(),
                    });
                }
            }
        }
        Ok(links)
    }
}

#[cfg(test)]
mod tests {
    use crate::model::ManageScope;
    use crate::test_support::seeded_store;
    use crate::DiscoveryEngine;

    fn engine() -> (tempfile::TempDir, DiscoveryEngine) {
        let (dir, store) = seeded_store();
        let engine = DiscoveryEngine::from_store(&store).unwrap();
        (dir, engine)
    }

    #[test]
    fn company_stakeholders_jmts() {
        let (_d, e) = engine();
        // PRD checkpoint: JMTS stakeholders are Tyler (segment_lead), Tracy
        // (exec), Brian (ops), Andrea (financial), Matt (tech).
        let s = e.company_stakeholders("JMTS").unwrap();
        assert_eq!(s.len(), 5);

        let by_email: std::collections::HashMap<&str, &ManageScope> =
            s.iter().map(|sh| (sh.person.email(), &sh.scope)).collect();
        assert_eq!(
            by_email.get("tyler.begemann@kanbrick.com"),
            Some(&&ManageScope::SegmentLead)
        );
        assert_eq!(
            by_email.get("tracy.brittcool@kanbrick.com"),
            Some(&&ManageScope::ExecutiveOversight)
        );
        assert_eq!(
            by_email.get("brian.humphrey@kanbrick.com"),
            Some(&&ManageScope::OperationalOversight)
        );
        assert_eq!(
            by_email.get("andrea.lewis@kanbrick.com"),
            Some(&&ManageScope::FinancialOversight)
        );
        assert_eq!(
            by_email.get("matt.berns@kanbrick.com"),
            Some(&&ManageScope::TechnologyOversight)
        );
    }

    #[test]
    fn company_stakeholders_second_company_and_missing() {
        let (_d, e) = engine();
        // BWK (Strategic Programs): Tracy, Brian, Andrea, Matt + Sloan (program_lead).
        let bwk = e.company_stakeholders("BWK").unwrap();
        assert_eq!(bwk.len(), 5);
        assert!(bwk
            .iter()
            .any(|s| s.person.email() == "sloan.allen@kanbrick.com"
                && s.scope == ManageScope::ProgramLead));

        // Non-existent company → empty.
        assert!(e.company_stakeholders("NOPE").unwrap().is_empty());
    }

    #[test]
    fn segment_overview_covers_operating_segments() {
        let (_d, e) = engine();

        // Testing & Lab Services: 5 companies, lead Tyler.
        let tls = e.segment_overview("TLS").unwrap();
        assert_eq!(tls.companies.len(), 5);
        assert_eq!(tls.lead.as_ref().unwrap().full_name, "Tyler Begemann");
        assert!(tls.persons.iter().any(|p| p.full_name == "Tyler Begemann"));

        // Industrial Distribution: KEEP + ASI, lead Blake.
        let ind = e.segment_overview("IND").unwrap();
        assert_eq!(ind.companies.len(), 2);
        assert_eq!(ind.lead.as_ref().unwrap().full_name, "Blake Richardson");

        // Manufacturing: DFPG, also led by Blake.
        let mfg = e.segment_overview("MFG").unwrap();
        assert_eq!(mfg.companies.len(), 1);
        assert_eq!(mfg.lead.as_ref().unwrap().full_name, "Blake Richardson");

        assert!(e.segment_overview("ZZZ").is_err());
    }

    #[test]
    fn cross_segment_links_surface_multi_segment_managers() {
        let (_d, e) = engine();
        let links = e.cross_segment_links().unwrap();
        let by_email: std::collections::HashMap<&str, usize> = links
            .iter()
            .map(|l| (l.person.email(), l.segments.len()))
            .collect();

        // The firm-wide oversight execs touch all four segments.
        for exec in [
            "tracy.brittcool@kanbrick.com",
            "brian.humphrey@kanbrick.com",
            "andrea.lewis@kanbrick.com",
            "matt.berns@kanbrick.com",
        ] {
            assert_eq!(by_email.get(exec), Some(&4));
        }
        // Blake spans Industrial Distribution + Manufacturing.
        assert_eq!(by_email.get("blake.richardson@kanbrick.com"), Some(&2));
        // Single-segment leads do not appear.
        assert!(!by_email.contains_key("tyler.begemann@kanbrick.com"));
        assert!(!by_email.contains_key("sloan.allen@kanbrick.com"));
    }
}
