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

use kanbrick_core::abi::{GraphQuery, GraphRows};
use kanbrick_core::{Error, FirmContext, Result};
use kanbrick_store::{CompanyNode, Params, PersonNode, Store};
use serde_json::Value as JsonValue;

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

    /// Run a **generic** graph query on behalf of a WASM guest (issue #24), audit
    /// it, and return the clearance-filtered rows as [`GraphRows`].
    ///
    /// This is the single choke point a guest's `query_graph` host call routes
    /// through. Parameters are bound (never interpolated), the query text is
    /// audited, and every returned row is passed through
    /// [`ClearanceScope::retain_rows`] — the fail-closed generic filter: a person
    /// row (`email`) or company row (`company_id`) is kept only if visible to the
    /// caller, and a projection exposing neither key is denied for non-L4/L5
    /// callers. A guest therefore can never see data above its caller's clearance.
    pub fn query_graph(&self, query: &GraphQuery) -> Result<GraphRows> {
        AuditLog::new(self.store).record(self.ctx, &query.cypher)?;
        let params = json_params(&query.params)?;
        let rows: Vec<JsonValue> = self.store.query(&query.cypher, params)?;
        let filtered = self.scope.retain_rows(rows)?;
        Ok(GraphRows::new(filtered))
    }
}

/// Lower JSON query parameters into the store's [`Params`]. Supports the scalar
/// types a guest binds in practice — string, integer, float, bool — and rejects
/// anything else (null, arrays, objects) as invalid input.
fn json_params<'a>(
    params: impl IntoIterator<Item = (&'a String, &'a JsonValue)>,
) -> Result<Params> {
    let mut out = Params::new();
    for (name, value) in params {
        match value {
            JsonValue::String(s) => out.insert(name, s.as_str()),
            JsonValue::Bool(b) => out.insert(name, *b),
            JsonValue::Number(n) => {
                if let Some(i) = n.as_i64() {
                    out.insert(name, i);
                } else if let Some(f) = n.as_f64() {
                    out.insert(name, f);
                } else {
                    return Err(Error::InvalidInput(format!(
                        "query parameter {name:?} has an unrepresentable number"
                    )));
                }
            }
            other => {
                return Err(Error::InvalidInput(format!(
                    "query parameter {name:?} has unsupported type {}",
                    json_type_name(other)
                )));
            }
        }
    }
    Ok(out)
}

/// A short type name for an unsupported JSON parameter value (for error text).
fn json_type_name(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "bool",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
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

    // ---- #24: the generic, fail-closed query_graph used by WASM guests. ----

    use kanbrick_core::abi::GraphQuery;

    // A *detail* projection (carries the non-public `description`), so it is
    // clearance-gated rather than the public company roster (ADR-0005).
    const ALL_COMPANY_DETAIL: &str = "MATCH (c:Company) RETURN c.company_id, c.name, c.description";
    // The public company roster: company identity only (ADR-0005).
    const COMPANY_ROSTER: &str = "MATCH (c:Company) RETURN c.company_id, c.name, c.segment";

    #[test]
    fn query_graph_l5_sees_every_row() {
        let (_d, store) = seeded();
        let ceo = ctx_l5();
        let guarded = GuardedStore::new(&store, &ceo).unwrap();
        let rows = guarded
            .query_graph(&GraphQuery::new(ALL_COMPANY_DETAIL))
            .unwrap();
        assert_eq!(rows.len(), 9);
    }

    #[test]
    fn query_graph_l3_detail_is_clearance_filtered_to_their_segment() {
        let (_d, store) = seeded();
        let lead = ctx("tyler.begemann@kanbrick.com", ClearanceLevel::L3);
        let guarded = GuardedStore::new(&store, &lead).unwrap();
        // The same all-companies *detail* query a guest issues comes back filtered
        // to the caller's 5 segment companies — never the other 4.
        let rows = guarded
            .query_graph(&GraphQuery::new(ALL_COMPANY_DETAIL))
            .unwrap();
        assert_eq!(rows.len(), 5);
        for row in &rows.rows {
            let id = row["company_id"].as_str().unwrap();
            assert!(["JMTS", "MCON", "AAG", "LTI", "ATS"].contains(&id));
        }
    }

    #[test]
    fn query_graph_public_roster_is_visible_to_every_clearance() {
        // PUBLIC_DATA (ADR-0005): company identity is readable by all tiers. An L1
        // who manages nothing still reads the full 9-company roster.
        let (_d, store) = seeded();
        let l1 = ctx("dana.prescott@kanbrick.com", ClearanceLevel::L1);
        let guarded = GuardedStore::new(&store, &l1).unwrap();
        let rows = guarded
            .query_graph(&GraphQuery::new(COMPANY_ROSTER))
            .unwrap();
        assert_eq!(rows.len(), 9);
    }

    #[test]
    fn query_graph_fails_closed_on_a_sensitive_unfilterable_projection() {
        let (_d, store) = seeded();
        let lead = ctx("tyler.begemann@kanbrick.com", ClearanceLevel::L3);
        let guarded = GuardedStore::new(&store, &lead).unwrap();
        // A *sensitive* projection exposing neither `email` nor `company_id` cannot
        // be proven safe, so a non-see-all caller is denied outright. (`c.name`
        // alone would be the public roster; `c.description` is gated.)
        let err = guarded
            .query_graph(&GraphQuery::new("MATCH (c:Company) RETURN c.description"))
            .unwrap_err();
        assert_eq!(err.kind(), kanbrick_core::ErrorKind::Unauthorized);
    }

    #[test]
    fn query_graph_binds_parameters_and_audits() {
        let (_d, store) = seeded();
        let ceo = ctx_l5();
        let guarded = GuardedStore::new(&store, &ceo).unwrap();
        // A bound parameter selects a single company by id (injection-safe).
        let rows = guarded
            .query_graph(
                &GraphQuery::new(
                    "MATCH (c:Company {company_id: $cid}) RETURN c.company_id, c.name",
                )
                .param("cid", "KEEP"),
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows.rows[0]["company_id"], serde_json::json!("KEEP"));

        let audited = AuditLog::new(&store).count_for_user(ceo.user_id).unwrap();
        assert_eq!(audited, 1, "the generic query is audited like any other");
    }

    #[test]
    fn query_graph_rejects_unsupported_parameter_types() {
        let (_d, store) = seeded();
        let ceo = ctx_l5();
        let guarded = GuardedStore::new(&store, &ceo).unwrap();
        let err = guarded
            .query_graph(
                &GraphQuery::new("MATCH (c:Company) RETURN c.company_id")
                    .param("bad", serde_json::json!(["array", "value"])),
            )
            .unwrap_err();
        assert_eq!(err.kind(), kanbrick_core::ErrorKind::ValidationError);
    }
}
