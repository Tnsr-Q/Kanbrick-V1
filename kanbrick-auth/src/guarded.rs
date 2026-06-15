//! Clearance-enforcing query interceptor (issue #18).
//!
//! [`GuardedStore`] wraps the store with a caller's [`FirmContext`] and is the
//! single choke point for authenticated reads: every query it runs is
//! **audited** (issue #19) and every result it returns is **clearance-filtered**
//! (issue #17).
//!
//! The PRD framed this as "auto-inject clearance `WHERE` filters into Cypher".
//! Per ADR-0001, SparrowDB's `WHERE` engine is unreliable in the pinned build,
//! and applying the filter in audited Rust is the safer design for a security
//! boundary — so the interceptor filters the returned rows rather than rewriting
//! the query text. The external contract (callers cannot see data above their
//! clearance) is identical.

use kanbrick_core::{FirmContext, Result};
use kanbrick_store::{CompanyNode, Params, PersonNode, Store};

use crate::audit::AuditLog;
use crate::scope::ClearanceScope;

/// A store handle bound to one caller, enforcing audit + clearance scoping.
pub struct GuardedStore<'a> {
    store: &'a Store,
    ctx: &'a FirmContext,
    scope: ClearanceScope,
}

impl<'a> GuardedStore<'a> {
    /// Bind the store to `ctx`, resolving the caller's clearance scope.
    pub fn new(store: &'a Store, ctx: &'a FirmContext) -> Result<Self> {
        let scope = ClearanceScope::resolve(store, ctx)?;
        Ok(GuardedStore { store, ctx, scope })
    }

    /// The resolved clearance scope for this caller.
    pub fn scope(&self) -> &ClearanceScope {
        &self.scope
    }

    /// Run a person query, audit it, and return only the rows this caller may
    /// see. The projection must expose the `PersonNode` fields un-aliased.
    pub fn query_persons(&self, cypher: &str, params: Params) -> Result<Vec<PersonNode>> {
        AuditLog::new(self.store).record(self.ctx, cypher)?;
        let rows: Vec<PersonNode> = self.store.query(cypher, params)?;
        Ok(self.scope.retain_persons(rows))
    }

    /// Run a company query, audit it, and return only the rows this caller may
    /// see. The projection must expose the `CompanyNode` fields un-aliased.
    pub fn query_companies(&self, cypher: &str, params: Params) -> Result<Vec<CompanyNode>> {
        AuditLog::new(self.store).record(self.ctx, cypher)?;
        let rows: Vec<CompanyNode> = self.store.query(cypher, params)?;
        Ok(self.scope.retain_companies(rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanbrick_core::ClearanceLevel;
    use kanbrick_store::Migrator;
    use uuid::Uuid;

    const ALL_COMPANIES: &str = "MATCH (c:Company) RETURN c.company_id, c.name, c.legal_name, \
        c.segment, c.status, c.acquired_year, c.hq_state, c.description";

    const ALL_PERSONS: &str = "MATCH (p:Person) RETURN p.full_name, p.first_name, p.last_name, \
        p.email, p.title, p.role, p.clearance_level, p.clearance_label, p.department, p.status, \
        p.segment, p.note";

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
        FirmContext::new(Uuid::new_v4(), email, clearance)
    }

    #[test]
    fn l3_company_query_is_scoped_to_their_segment() {
        let (_d, store) = seeded();
        let ctx = ctx("tyler.begemann@kanbrick.com", ClearanceLevel::L3);
        let guarded = GuardedStore::new(&store, &ctx).unwrap();

        // The *same* "all companies" query returns only the caller's 5.
        let visible = guarded
            .query_companies(ALL_COMPANIES, Params::new())
            .unwrap();
        assert_eq!(visible.len(), 5);

        // L5 sees all 9 through the same interceptor.
        let ceo = ctx_l5();
        let guarded5 = GuardedStore::new(&store, &ceo).unwrap();
        assert_eq!(
            guarded5
                .query_companies(ALL_COMPANIES, Params::new())
                .unwrap()
                .len(),
            9
        );
    }

    fn ctx_l5() -> FirmContext {
        ctx("tracy.brittcool@kanbrick.com", ClearanceLevel::L5)
    }

    #[test]
    fn every_guarded_query_is_audited() {
        let (_d, store) = seeded();
        let ctx = ctx("elena.ruiz@kanbrick.com", ClearanceLevel::L2);
        let guarded = GuardedStore::new(&store, &ctx).unwrap();

        guarded
            .query_companies(ALL_COMPANIES, Params::new())
            .unwrap();

        // An L2 analyst querying all persons sees only themselves.
        let persons = guarded.query_persons(ALL_PERSONS, Params::new()).unwrap();
        assert_eq!(persons.len(), 1);
        assert_eq!(persons[0].email, "elena.ruiz@kanbrick.com");

        let audited = AuditLog::new(&store).count_for_user(ctx.user_id).unwrap();
        assert_eq!(audited, 2, "each guarded query records one audit entry");
    }
}
