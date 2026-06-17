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

use kanbrick_core::{ClearanceLevel, Error, FirmContext, Result};
use kanbrick_store::{CompanyNode, Params, PersonNode, Store};
use serde::Deserialize;
use serde_json::Value as JsonValue;

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

    /// Fail-closed clearance filter for generic JSON result rows (issue #24).
    ///
    /// Each row is classified by the security-relevant keys it exposes:
    ///
    /// * a **person row** (`email`) is kept iff the caller may see that person;
    /// * a **public roster row** — every projected key is a
    ///   [`PUBLIC_COMPANY_FIELDS`] entry (`company_id`/`name`/`segment`) — is kept
    ///   for **everyone**: company identity is public (`PUBLIC_DATA`, ADR-0005);
    /// * a **company detail row** (`company_id` plus any non-public field) is kept
    ///   iff the caller may see that company;
    /// * any other row — a sensitive projection exposing no clearance key — is
    ///   **denied** (fail-closed): a guest cannot dodge filtering by projecting raw
    ///   columns. L4/L5 callers see every row unfiltered.
    pub fn retain_rows(&self, rows: Vec<JsonValue>) -> Result<Vec<JsonValue>> {
        if self.sees_all {
            return Ok(rows);
        }
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            if self.row_is_visible(&row)? {
                out.push(row);
            }
        }
        Ok(out)
    }

    /// Decide whether one generic result row is visible to this (non-see-all)
    /// caller: `Ok(true)` keep, `Ok(false)` drop, `Err` deny the whole query.
    fn row_is_visible(&self, row: &JsonValue) -> Result<bool> {
        let denied = || Error::AccessDenied {
            required: ClearanceLevel::L4,
            actual: self.clearance,
        };
        let obj = row.as_object().ok_or_else(denied)?;

        if let Some(email) = obj.get("email").and_then(JsonValue::as_str) {
            return Ok(self.can_see_person(email));
        }
        // Public roster: company identity (company_id/name/segment) is readable by
        // every clearance (ADR-0005). A row projecting only those is always kept.
        if !obj.is_empty()
            && obj
                .keys()
                .all(|k| PUBLIC_COMPANY_FIELDS.contains(&k.as_str()))
        {
            return Ok(true);
        }
        if let Some(company_id) = obj.get("company_id").and_then(JsonValue::as_str) {
            return Ok(self.can_see_company(company_id));
        }
        // Sensitive projection with no clearance key: cannot be proven safe.
        Err(denied())
    }
}

/// Company fields that are **public** — the portfolio roster identity, readable
/// by every clearance (`PUBLIC_DATA`, ADR-0005). All other company fields, and
/// all person and financial data, remain clearance-gated.
pub const PUBLIC_COMPANY_FIELDS: &[&str] = &["company_id", "name", "segment"];

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

    // ---- public company roster (PUBLIC_DATA, ADR-0005). ----

    fn roster_rows() -> Vec<JsonValue> {
        vec![
            serde_json::json!({"company_id": "JMTS", "name": "JM Test Systems", "segment": "Testing & Lab Services"}),
            serde_json::json!({"company_id": "KEEP", "name": "Keep Supply", "segment": "Industrial Distribution"}),
        ]
    }

    #[test]
    fn l1_sees_the_full_public_company_roster() {
        let (_d, store) = seeded();
        // Dana is L1 and manages no companies — yet company identity is public.
        let scope = ClearanceScope::resolve(
            &store,
            &ctx("dana.prescott@kanbrick.com", ClearanceLevel::L1),
        )
        .unwrap();
        assert!(!scope.sees_all());
        let kept = scope.retain_rows(roster_rows()).unwrap();
        assert_eq!(kept.len(), 2, "L1 reads the public company roster");
    }

    #[test]
    fn company_detail_projection_stays_gated() {
        let (_d, store) = seeded();
        // Tyler (L3) sees only his 5 segment companies in *detail*.
        let scope = ClearanceScope::resolve(
            &store,
            &ctx("tyler.begemann@kanbrick.com", ClearanceLevel::L3),
        )
        .unwrap();
        // A detail row carries a non-public field (description) → gated.
        let rows = vec![
            serde_json::json!({"company_id": "JMTS", "name": "JM Test Systems", "description": "x"}),
            serde_json::json!({"company_id": "KEEP", "name": "Keep Supply", "description": "y"}),
        ];
        let kept = scope.retain_rows(rows).unwrap();
        assert_eq!(
            kept.len(),
            1,
            "only the in-segment company's detail is kept"
        );
        assert_eq!(kept[0]["company_id"], "JMTS");
    }

    #[test]
    fn sensitive_projection_without_a_key_is_denied() {
        let (_d, store) = seeded();
        let scope = ClearanceScope::resolve(
            &store,
            &ctx("tyler.begemann@kanbrick.com", ClearanceLevel::L3),
        )
        .unwrap();
        // description alone is sensitive and carries no clearance key → fail-closed.
        let err = scope
            .retain_rows(vec![serde_json::json!({"description": "secret"})])
            .unwrap_err();
        assert_eq!(err.kind(), kanbrick_core::ErrorKind::Unauthorized);
    }

    #[test]
    fn person_rows_remain_gated_despite_public_companies() {
        let (_d, store) = seeded();
        let scope = ClearanceScope::resolve(
            &store,
            &ctx("dana.prescott@kanbrick.com", ClearanceLevel::L1),
        )
        .unwrap();
        // Personnel are not public: an L1 sees only their own person row.
        let rows = vec![
            serde_json::json!({"email": "dana.prescott@kanbrick.com"}),
            serde_json::json!({"email": "tracy.brittcool@kanbrick.com"}),
        ];
        let kept = scope.retain_rows(rows).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0]["email"], "dana.prescott@kanbrick.com");
    }
}
