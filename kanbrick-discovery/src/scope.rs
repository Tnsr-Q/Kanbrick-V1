//! Clearance-aware discovery: analytics are privileged, answers are scoped
//! (issue #36, ADR-0003 §4–§5).
//!
//! The discovery analysis runs over the **full** firm graph; the *results* are
//! then filtered to the caller's [`VisibilityScope`] so an answer never reveals a
//! node the caller could not have seen via a normal clearance-filtered query:
//!
//! * **paths** are all-or-nothing (never partial → no gaps that imply a hidden
//!   node),
//! * **node lists** are filtered to visible members and gated on a visible
//!   subject,
//! * **scalar metrics** are answered only for a visible subject (and are the true
//!   global value — an aggregate that names no hidden individual),
//! * **single-node answers** require the answer and both subjects to be visible.
//!
//! ## Extensible, composable scopes (operator-directed)
//!
//! Visibility is a trait, not the hard-wired L1–L5 ladder.
//! [`kanbrick_auth::ClearanceScope`] is one implementation (static clearance). A
//! [`ProjectScope`] composes **additive** grants on top of any base scope: an
//! employee can be granted extra, project-scoped visibility (the foundation for
//! per-project AI agents/skills) without changing their clearance, and a grant
//! can only *add* visibility, never remove the base. The request → approval →
//! grant workflow that authorizes creating a `ProjectScope` is tracked
//! separately (see the Phase 4 follow-up issue); this module is the enforcement
//! primitive it builds on.

use std::collections::BTreeSet;

use kanbrick_auth::ClearanceScope;
use kanbrick_core::Result;

use crate::model::{
    CompanyRef, CrossSegmentLink, OrgNeighborhood, PersonRef, ReportingPath, SegmentReport,
    SpanMetrics, Stakeholder,
};
use crate::DiscoveryEngine;

/// A caller's data-visibility over the firm graph.
///
/// Implementations answer "may this caller see this node?". The discovery engine
/// filters every answer through one of these.
pub trait VisibilityScope: std::fmt::Debug {
    /// Whether the caller sees every node unfiltered (L4/L5).
    fn sees_all(&self) -> bool;

    /// Whether the caller may see the person with this email.
    fn can_see_person(&self, email: &str) -> bool;

    /// Whether the caller may see the company with this code.
    fn can_see_company(&self, company_id: &str) -> bool;

    /// A stable identity for cache-keying: two scopes with equal keys see exactly
    /// the same nodes (used by [`crate::cache`] so an L3 entry never serves an L5
    /// caller).
    fn scope_key(&self) -> String;
}

impl VisibilityScope for ClearanceScope {
    fn sees_all(&self) -> bool {
        ClearanceScope::sees_all(self)
    }

    fn can_see_person(&self, email: &str) -> bool {
        ClearanceScope::can_see_person(self, email)
    }

    fn can_see_company(&self, company_id: &str) -> bool {
        ClearanceScope::can_see_company(self, company_id)
    }

    fn scope_key(&self) -> String {
        // Within one graph snapshot, (clearance, identity) fixes visibility.
        format!("clearance:{}:{}", self.clearance(), self.self_email())
    }
}

/// An additive, project-scoped grant layered on top of a base [`VisibilityScope`].
///
/// A grant only ever *adds* visibility (union with the base); it can never remove
/// what the base already allows, nor elevate the caller to "sees all". Compose
/// freely — a `ProjectScope` is itself a [`VisibilityScope`].
#[derive(Debug)]
pub struct ProjectScope {
    base: Box<dyn VisibilityScope>,
    project: String,
    granted_persons: BTreeSet<String>,
    granted_companies: BTreeSet<String>,
}

impl ProjectScope {
    /// Start a project scope for `project` over `base`, with no grants yet.
    pub fn new(base: impl VisibilityScope + 'static, project: impl Into<String>) -> Self {
        ProjectScope {
            base: Box::new(base),
            project: project.into(),
            granted_persons: BTreeSet::new(),
            granted_companies: BTreeSet::new(),
        }
    }

    /// Grant visibility of a person (by email).
    pub fn grant_person(mut self, email: impl Into<String>) -> Self {
        self.granted_persons.insert(email.into());
        self
    }

    /// Grant visibility of a company (by code).
    pub fn grant_company(mut self, company_id: impl Into<String>) -> Self {
        self.granted_companies.insert(company_id.into());
        self
    }

    /// Grant visibility of several people at once.
    pub fn grant_persons<I, S>(mut self, emails: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.granted_persons
            .extend(emails.into_iter().map(Into::into));
        self
    }

    /// Grant visibility of several companies at once.
    pub fn grant_companies<I, S>(mut self, ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.granted_companies
            .extend(ids.into_iter().map(Into::into));
        self
    }

    /// The project this scope is bound to.
    pub fn project(&self) -> &str {
        &self.project
    }
}

impl VisibilityScope for ProjectScope {
    fn sees_all(&self) -> bool {
        // A grant never elevates to "see all"; that stays the base's decision.
        self.base.sees_all()
    }

    fn can_see_person(&self, email: &str) -> bool {
        self.base.can_see_person(email) || self.granted_persons.contains(email)
    }

    fn can_see_company(&self, company_id: &str) -> bool {
        self.base.can_see_company(company_id) || self.granted_companies.contains(company_id)
    }

    fn scope_key(&self) -> String {
        format!(
            "project:{}:{}:p[{}]:c[{}]",
            self.project,
            self.base.scope_key(),
            self.granted_persons
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(","),
            self.granted_companies
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(","),
        )
    }
}

impl DiscoveryEngine {
    /// [`reporting_path`](Self::reporting_path), filtered: returned only if every
    /// person on it is visible to `scope` (all-or-nothing — no partial paths).
    pub fn scoped_reporting_path(
        &self,
        scope: &dyn VisibilityScope,
        from: &str,
        to: &str,
    ) -> Result<Option<ReportingPath>> {
        let Some(path) = self.reporting_path(from, to)? else {
            return Ok(None);
        };
        if path.steps.iter().all(|p| scope.can_see_person(p.email())) {
            Ok(Some(path))
        } else {
            Ok(None)
        }
    }

    /// [`span_of_control`](Self::span_of_control) for a person `scope` may see;
    /// `None` otherwise. The metric itself is the true global value.
    pub fn scoped_span_of_control(
        &self,
        scope: &dyn VisibilityScope,
        person: &str,
    ) -> Result<Option<SpanMetrics>> {
        let Some(p) = self.graph.resolve_person(person) else {
            return Ok(None);
        };
        if !scope.can_see_person(&p.email) {
            return Ok(None);
        }
        Ok(Some(self.span_of_control(person)?))
    }

    /// [`org_neighborhood`](Self::org_neighborhood) centred on a visible person,
    /// with members and edges filtered to what `scope` may see. `None` when the
    /// centre is not visible.
    pub fn scoped_org_neighborhood(
        &self,
        scope: &dyn VisibilityScope,
        person: &str,
        depth: usize,
    ) -> Result<Option<OrgNeighborhood>> {
        let Some(center) = self.graph.resolve_person(person) else {
            return Ok(None);
        };
        if !scope.can_see_person(&center.email) {
            return Ok(None);
        }
        let mut hood = self.org_neighborhood(person, depth)?;
        let visible: BTreeSet<String> = hood
            .members
            .iter()
            .filter(|m| scope.can_see_person(m.email()))
            .map(|m| m.id.as_str().to_string())
            .collect();
        hood.members.retain(|m| visible.contains(m.id.as_str()));
        hood.reporting_edges
            .retain(|(a, b)| visible.contains(a.as_str()) && visible.contains(b.as_str()));
        Ok(Some(hood))
    }

    /// [`common_manager`](Self::common_manager), returned only if the answer and
    /// both subjects are visible to `scope`.
    pub fn scoped_common_manager(
        &self,
        scope: &dyn VisibilityScope,
        a: &str,
        b: &str,
    ) -> Result<Option<PersonRef>> {
        let (Some(pa), Some(pb)) = (self.graph.resolve_person(a), self.graph.resolve_person(b))
        else {
            return Ok(None);
        };
        if !scope.can_see_person(&pa.email) || !scope.can_see_person(&pb.email) {
            return Ok(None);
        }
        match self.common_manager(a, b)? {
            Some(mgr) if scope.can_see_person(mgr.email()) => Ok(Some(mgr)),
            _ => Ok(None),
        }
    }

    /// [`company_stakeholders`](Self::company_stakeholders), gated on a visible
    /// company and filtered to visible stakeholders.
    pub fn scoped_company_stakeholders(
        &self,
        scope: &dyn VisibilityScope,
        company: &str,
    ) -> Result<Vec<Stakeholder>> {
        let Some(c) = self.graph.resolve_company(company) else {
            return Ok(Vec::new());
        };
        if !scope.can_see_company(&c.company_id) {
            return Ok(Vec::new());
        }
        Ok(self
            .company_stakeholders(company)?
            .into_iter()
            .filter(|s| scope.can_see_person(s.person.email()))
            .collect())
    }

    /// [`segment_overview`](Self::segment_overview), with companies, persons, and
    /// lead filtered to what `scope` may see. `None` when the caller can see no
    /// company in the segment.
    pub fn scoped_segment_overview(
        &self,
        scope: &dyn VisibilityScope,
        segment: &str,
    ) -> Result<Option<SegmentReport>> {
        let mut report = self.segment_overview(segment)?;

        let any_visible_company = scope.sees_all()
            || report
                .companies
                .iter()
                .any(|c| scope.can_see_company(c.id.as_str()));
        if !any_visible_company {
            return Ok(None);
        }

        report
            .companies
            .retain(|c: &CompanyRef| scope.can_see_company(c.id.as_str()));
        report.persons.retain(|p| scope.can_see_person(p.email()));
        report.lead = report.lead.filter(|l| scope.can_see_person(l.email()));
        Ok(Some(report))
    }

    /// [`cross_segment_links`](Self::cross_segment_links), filtered to people
    /// `scope` may see.
    pub fn scoped_cross_segment_links(
        &self,
        scope: &dyn VisibilityScope,
    ) -> Result<Vec<CrossSegmentLink>> {
        Ok(self
            .cross_segment_links()?
            .into_iter()
            .filter(|l| scope.can_see_person(l.person.email()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{ctx, seeded_store};
    use crate::DiscoveryEngine;
    use kanbrick_auth::ClearanceScope;
    use kanbrick_core::ClearanceLevel;
    use kanbrick_store::Store;

    fn setup() -> (tempfile::TempDir, Store, DiscoveryEngine) {
        let (dir, store) = seeded_store();
        let engine = DiscoveryEngine::from_store(&store).unwrap();
        (dir, store, engine)
    }

    fn scope_for(store: &Store, email: &str, level: ClearanceLevel) -> ClearanceScope {
        ClearanceScope::resolve(store, &ctx(email, level)).unwrap()
    }

    #[test]
    fn l5_sees_complete_results() {
        let (_d, store, e) = setup();
        let l5 = scope_for(&store, "tracy.brittcool@kanbrick.com", ClearanceLevel::L5);

        // Full path, all stakeholders, true span — nothing filtered.
        let path = e
            .scoped_reporting_path(&l5, "samantha.jordan", "tracy.brittcool")
            .unwrap()
            .unwrap();
        assert_eq!(path.len(), 4);
        assert_eq!(e.scoped_company_stakeholders(&l5, "JMTS").unwrap().len(), 5);
        assert_eq!(
            e.scoped_span_of_control(&l5, "tracy.brittcool")
                .unwrap()
                .unwrap()
                .indirect_reports,
            11
        );
    }

    #[test]
    fn l3_limited_to_own_segment_and_reports() {
        let (_d, store, e) = setup();
        // Tyler leads Testing & Lab Services; sees his 5 companies + self + Samantha.
        let l3 = scope_for(&store, "tyler.begemann@kanbrick.com", ClearanceLevel::L3);

        // He can see JMTS, but among its stakeholders only himself (not the
        // L4/L5 execs who also manage it).
        let jmts = e.scoped_company_stakeholders(&l3, "JMTS").unwrap();
        assert_eq!(jmts.len(), 1);
        assert_eq!(jmts[0].person.email(), "tyler.begemann@kanbrick.com");

        // A company outside his segment is invisible.
        assert!(e
            .scoped_company_stakeholders(&l3, "KEEP")
            .unwrap()
            .is_empty());

        // Path up to the CEO crosses people he cannot see → withheld entirely.
        assert!(e
            .scoped_reporting_path(&l3, "tyler.begemann", "tracy.brittcool")
            .unwrap()
            .is_none());
        // Path to his own report is fine.
        assert!(e
            .scoped_reporting_path(&l3, "samantha.jordan", "tyler.begemann")
            .unwrap()
            .is_some());
    }

    #[test]
    fn l2_cannot_discover_higher_clearance_details() {
        let (_d, store, e) = setup();
        // Elena (L2) sees only herself.
        let l2 = scope_for(&store, "elena.ruiz@kanbrick.com", ClearanceLevel::L2);

        // Cannot trace to, or read the span of, an L5.
        assert!(e
            .scoped_reporting_path(&l2, "elena.ruiz", "tracy.brittcool")
            .unwrap()
            .is_none());
        assert!(e
            .scoped_span_of_control(&l2, "tracy.brittcool")
            .unwrap()
            .is_none());
        // Cannot discover the CEO via a common-manager query either.
        assert!(e
            .scoped_common_manager(&l2, "elena.ruiz", "samantha.jordan")
            .unwrap()
            .is_none());
    }

    #[test]
    fn same_query_differs_by_clearance() {
        let (_d, store, e) = setup();
        let l5 = scope_for(&store, "tracy.brittcool@kanbrick.com", ClearanceLevel::L5);
        let l3 = scope_for(&store, "tyler.begemann@kanbrick.com", ClearanceLevel::L3);

        let l5_jmts = e.scoped_company_stakeholders(&l5, "JMTS").unwrap();
        let l3_jmts = e.scoped_company_stakeholders(&l3, "JMTS").unwrap();
        assert_ne!(l5_jmts.len(), l3_jmts.len());
    }

    #[test]
    fn project_scope_grants_are_additive() {
        let (_d, store, e) = setup();
        let base = scope_for(&store, "elena.ruiz@kanbrick.com", ClearanceLevel::L2);
        let base_key = VisibilityScope::scope_key(&base);

        // Elena is granted a project scope onto JMTS + Tyler.
        let project = ProjectScope::new(base, "valuation-jmts")
            .grant_company("JMTS")
            .grant_person("tyler.begemann@kanbrick.com");

        // She can now see JMTS and Tyler among its stakeholders — but still not
        // the L4/L5 execs (the grant only *added* what was granted).
        let jmts = e.scoped_company_stakeholders(&project, "JMTS").unwrap();
        assert_eq!(jmts.len(), 1);
        assert_eq!(jmts[0].person.email(), "tyler.begemann@kanbrick.com");

        // A different company she was not granted stays invisible.
        assert!(e
            .scoped_company_stakeholders(&project, "KEEP")
            .unwrap()
            .is_empty());

        // The grant changes the cache identity (so it can never serve an
        // ungranted caller from cache).
        assert_ne!(project.scope_key(), base_key);
    }
}
