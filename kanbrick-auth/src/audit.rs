//! Audit logging (issue #19).
//!
//! Every authenticated query is recorded as an `(:AuditEntry)` node carrying the
//! caller's `user_id` and `clearance`, a `query_hash` (SHA-256 of the query
//! text, so the log never stores raw query bodies), and an RFC 3339 `timestamp`.
//! Each entry has a unique `entry_id` so writes never collide.

use kanbrick_core::{FirmContext, Result};
use kanbrick_store::{Params, Store};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Writes audit entries for authenticated queries.
pub struct AuditLog<'a> {
    store: &'a Store,
}

impl<'a> AuditLog<'a> {
    /// Build an audit log backed by `store`.
    pub fn new(store: &'a Store) -> Self {
        AuditLog { store }
    }

    /// Record that `ctx` executed `query`. Returns the new entry's id.
    pub fn record(&self, ctx: &FirmContext, query: &str) -> Result<Uuid> {
        let entry_id = Uuid::new_v4();
        let params = Params::new()
            .with("entry_id", entry_id.to_string())
            .with("user_id", ctx.user_id.to_string())
            .with("clearance", ctx.clearance.to_string())
            .with("query_hash", query_hash(query))
            .with("timestamp", chrono::Utc::now().to_rfc3339());
        self.store.execute_with(
            "MERGE (a:AuditEntry {entry_id: $entry_id, user_id: $user_id, \
             clearance: $clearance, query_hash: $query_hash, timestamp: $timestamp})",
            params,
        )?;
        Ok(entry_id)
    }

    /// Count audit entries recorded for a given user id (test/inspection helper).
    pub fn count_for_user(&self, user_id: Uuid) -> Result<i64> {
        Ok(self
            .store
            .scalar_i64(
                "MATCH (a:AuditEntry {user_id: $user_id}) RETURN count(a)",
                Params::new().with("user_id", user_id.to_string()),
            )?
            .unwrap_or(0))
    }
}

/// SHA-256 of a query string, hex-encoded — the stored `query_hash`.
pub fn query_hash(query: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(query.as_bytes());
    let digest = hasher.finalize();
    let mut s = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(s, "{byte:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanbrick_core::ClearanceLevel;

    #[test]
    fn records_one_entry_per_query() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let audit = AuditLog::new(&store);
        let ctx = FirmContext::new(Uuid::new_v4(), "a@kanbrick.com", ClearanceLevel::L2);

        audit.record(&ctx, "MATCH (p:Person) RETURN p").unwrap();
        audit.record(&ctx, "MATCH (c:Company) RETURN c").unwrap();

        assert_eq!(audit.count_for_user(ctx.user_id).unwrap(), 2);
    }

    #[test]
    fn query_hash_is_stable_and_hides_query_text() {
        let h = query_hash("MATCH (p:Person) RETURN p");
        assert_eq!(h.len(), 64, "sha-256 hex is 64 chars");
        assert_eq!(h, query_hash("MATCH (p:Person) RETURN p"));
        assert_ne!(h, query_hash("MATCH (c:Company) RETURN c"));
    }
}
