//! Clearance-scoped data visibility (issue #17).
//!
//! Where [`crate::clearance`] answers "may this caller invoke this route at
//! all", this module answers "which rows and fields of the *result* may they
//! see". A [`ClearanceScope`] is resolved once per caller from the graph and
//! then applied to typed result sets.
//!
//! ## Model (HITL — issue #17)
//!
//! Per the PRD clearance model:
//!
//! * **L5 / L4** — see every person and company, all fields.
//! * **L3** — own segment's companies (the companies they *manage*) and their
//!   own direct + indirect reports (plus themselves).
//! * **L2** — their assigned companies (those they manage, if any) and their own
//!   record.
//! * **L1** — their own record and only public company fields.
//!
//! Sensitive person fields (e.g. compensation) are additionally gated: a caller
//! may see them only for persons at or below their own clearance. This is what
//! stops an L2 analyst from reading L4/L5 compensation.
//!
//! ## Why Rust and not Cypher `WHERE`
//!
//! Per ADR-0001, SparrowDB's `WHERE`/comparison engine is unreliable in the
//! pinned build, and — more importantly — keeping the clearance decision in
//! audited Rust is the safer design for a security boundary. So filtering is
//! applied here, over the rows SparrowDB returns, not by rewriting queries.

use std::collections::HashSet;

use kanbrick_core::{ClearanceLevel, FirmContext, Result};
use kanbrick_store::{CompanyNode, Params, PersonNode, Store};
use serde::Deserialize;

/// A caller's resolved data-visibility scope.
#[derive(Debug, Clone)]
pub struct ClearanceScope {
    clearance: ClearanceLevel,
    self_email: String,
    /// `true` for L4/L5 — sees everything, unfiltered.
    sees_all: bool,
    /// Company codes the caller may see in full (L2/L3).
    visible_company_ids: HashSet<String>,
    /// Person emails the caller may see (self + reports for L3).
    visible_person_emails: HashSet<String>,
}

#[derive(Debug, Deserialize)]
struct EmailRow {
    email: String,
}

#[derive(Debug, Deserialize)]
struct CompanyIdRow {
    company_id: String,
}

impl ClearanceScope {
    /// Resolve the scope for `ctx` against the graph.
    pub fn resolve(store: &Store, ctx: &FirmContext) -> Result<Self> {
        let clearance = ctx.clearance;
        let self_email = ctx.email.clone();
        let sees_all = clearance >= ClearanceLevel::L4;

        let mut visible_person_emails = HashSet::new();
        visible_person_emails.insert(self_email.clone());
        let mut visible_company_ids = HashSet::new();

        if !sees_all {
            // Companies the caller manages (their segment's companies for an L3
            // lead; assigned companies for an L2).
            let companies: Vec<CompanyIdRow> = store.query(
                "MATCH (p:Person {email: $email})-[:MANAGES]->(c:Company) RETURN c.company_id",
                Params::new().with("email", self_email.as_str()),
            )?;
            visible_company_ids.extend(companies.into_iter().map(|c| c.company_id));

            // Direct + indirect reports (native variable-length traversal).
            if clearance >= ClearanceLevel::L3 {
                let reports: Vec<EmailRow> = store.query(
                    "MATCH (sub:Person)-[:REPORTS_TO*1..10]->(p:Person {email: $email}) \
                     RETURN sub.email",
                    Params::new().with("email", self_email.as_str()),
                )?;
                visible_person_emails.extend(reports.into_iter().map(|r| r.email));
            }
        }

        Ok(ClearanceScope {
            clearance,
            self_email,
            sees_all,
            visible_company_ids,
            visible_person_emails,
        })
    }

    /// The caller's clearance.
    pub fn clearance(&self) -> ClearanceLevel {
        self.clearance
    }

    /// Whether the caller sees everything unfiltered (L4/L5).
    pub fn sees_all(&self) -> bool {
        self.sees_all
    }

    /// Whether the caller may see the person with `email`.
    pub fn can_see_person(&self, email: &str) -> bool {
        self.sees_all || self.visible_person_emails.contains(email)
    }

    /// Whether the caller may see the company with `company_id`.
    pub fn can_see_company(&self, company_id: &str) -> bool {
        self.sees_all || self.visible_company_ids.contains(company_id)
    }

    /// Whether the caller may read *sensitive* fields (e.g. compensation) of a
    /// person at `target` clearance. Allowed only when the caller's clearance is
    /// at least the target's — so an L2 cannot read L4/L5 sensitive data.
    pub fn can_view_sensitive_of(&self, target: ClearanceLevel) -> bool {
        self.sees_all || self.clearance >= target
    }

    /// Keep only the persons this scope may see.
    pub fn retain_persons(&self, persons: Vec<PersonNode>) -> Vec<PersonNode> {
        if self.sees_all {
            return persons;
        }
        persons
            .into_iter()
            .filter(|p| self.can_see_person(&p.email))
            .collect()
    }

    /// Keep only the companies this scope may see.
    pub fn retain_companies(&self, companies: Vec<CompanyNode>) -> Vec<CompanyNode> {
        if self.sees_all {
            return companies;
        }
        companies
            .into_iter()
            .filter(|c| self.can_see_company(&c.company_id))
            .collect()
    }

    /// The caller's own email (always visible to themselves).
    pub fn self_email(&self) -> &str {
        &self.self_email
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanbrick_store::Migrator;

    fn seeded() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let seed = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../seed/kanbrick_seed_data.cypher"
        ))
        .unwrap();
        Migrator::firm(seed).run(&store).unwrap();
        (dir, store)
    }

    fn ctx(email: &str, clearance: ClearanceLevel) -> FirmContext {
        FirmContext::new(uuid::Uuid::new_v4(), email, clearance)
    }

    fn all_companies(store: &Store) -> Vec<CompanyNode> {
        store
            .query(
                "MATCH (c:Company) RETURN c.company_id, c.name, c.legal_name, c.segment, \
                 c.status, c.acquired_year, c.hq_state, c.description",
                Params::new(),
            )
            .unwrap()
    }

    #[test]
    fn l5_sees_all_companies() {
        let (_d, store) = seeded();
        let scope = ClearanceScope::resolve(
            &store,
            &ctx("tracy.brittcool@kanbrick.com", ClearanceLevel::L5),
        )
        .unwrap();
        assert!(scope.sees_all());
        assert_eq!(scope.retain_companies(all_companies(&store)).len(), 9);
    }

    #[test]
    fn l3_lead_sees_only_their_segment_companies() {
        let (_d, store) = seeded();
        // Tyler leads Testing & Lab Services (5 companies).
        let scope = ClearanceScope::resolve(
            &store,
            &ctx("tyler.begemann@kanbrick.com", ClearanceLevel::L3),
        )
        .unwrap();
        assert!(!scope.sees_all());
        let visible = scope.retain_companies(all_companies(&store));
        assert_eq!(
            visible.len(),
            5,
            "L3 lead sees only their 5 segment companies"
        );
        assert!(visible
            .iter()
            .all(|c| ["JMTS", "MCON", "AAG", "LTI", "ATS"].contains(&c.company_id.as_str())));
        // A company outside the segment is not visible.
        assert!(!scope.can_see_company("KEEP"));
    }

    #[test]
    fn l3_lead_sees_self_and_reports() {
        let (_d, store) = seeded();
        let scope = ClearanceScope::resolve(
            &store,
            &ctx("tyler.begemann@kanbrick.com", ClearanceLevel::L3),
        )
        .unwrap();
        assert!(scope.can_see_person("tyler.begemann@kanbrick.com")); // self
        assert!(scope.can_see_person("samantha.jordan@kanbrick.com")); // direct report
        assert!(!scope.can_see_person("tracy.brittcool@kanbrick.com")); // the CEO
    }

    #[test]
    fn l2_analyst_sees_only_self_and_no_companies() {
        let (_d, store) = seeded();
        let scope =
            ClearanceScope::resolve(&store, &ctx("elena.ruiz@kanbrick.com", ClearanceLevel::L2))
                .unwrap();
        assert!(scope.can_see_person("elena.ruiz@kanbrick.com"));
        assert!(!scope.can_see_person("tracy.brittcool@kanbrick.com"));
        assert!(scope.retain_companies(all_companies(&store)).is_empty());
    }

    #[test]
    fn l2_cannot_view_sensitive_of_higher_clearance() {
        let (_d, store) = seeded();
        let scope =
            ClearanceScope::resolve(&store, &ctx("elena.ruiz@kanbrick.com", ClearanceLevel::L2))
                .unwrap();
        // Cannot read L4/L5 sensitive (compensation) fields...
        assert!(!scope.can_view_sensitive_of(ClearanceLevel::L4));
        assert!(!scope.can_view_sensitive_of(ClearanceLevel::L5));
        // ...but can read their own tier and below.
        assert!(scope.can_view_sensitive_of(ClearanceLevel::L2));
        assert!(scope.can_view_sensitive_of(ClearanceLevel::L1));
    }
}
