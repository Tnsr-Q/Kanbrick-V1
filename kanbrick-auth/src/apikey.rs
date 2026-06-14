//! Scoped API keys for service-to-service auth (issue #20).
//!
//! WASM guests authenticate to the host with an API key rather than a user JWT.
//! A key is bound to a service name and a fixed [`ClearanceLevel`], so a guest
//! can never act above the clearance it was provisioned with. Keys are stored as
//! `(:ApiKey)` nodes holding only a SHA-256 *hash* of the secret (never the
//! secret itself) plus an `active` flag, so rotation is a flip of that flag.
//!
//! A presented secret has the form `<key_id>.<random>`; validation parses the
//! `key_id`, loads that key, and constant-time-compares the hash.

use kanbrick_core::{ClearanceLevel, Error, FirmContext, Result};
use kanbrick_store::{Params, Store};
use serde::Deserialize;
use uuid::Uuid;

use crate::audit::query_hash;

/// A freshly issued API key. The `secret` is returned exactly once and is not
/// recoverable afterwards (only its hash is stored).
#[derive(Debug, Clone)]
pub struct IssuedKey {
    /// Stable identifier for the key (also embedded in the secret).
    pub key_id: Uuid,
    /// The full secret to present on requests. Show once, store nowhere.
    pub secret: String,
}

#[derive(Debug, Deserialize)]
struct KeyRow {
    key_hash: String,
    clearance: ClearanceLevel,
    service: String,
    // SparrowDB returns booleans as integers (see ADR-0001), so `active` is
    // stored and read as 1/0 rather than true/false.
    active: i64,
}

/// Issues, validates, and rotates scoped service API keys.
pub struct ApiKeyService<'a> {
    store: &'a Store,
}

impl<'a> ApiKeyService<'a> {
    /// Build the service over `store`.
    pub fn new(store: &'a Store) -> Self {
        ApiKeyService { store }
    }

    /// Issue a new key for `service` bound to `clearance`.
    pub fn issue(&self, service: &str, clearance: ClearanceLevel) -> Result<IssuedKey> {
        let key_id = Uuid::new_v4();
        let random = random_token();
        let secret = format!("{key_id}.{random}");
        let key_hash = query_hash(&secret);

        self.store.execute_with(
            "MERGE (a:ApiKey {key_id: $key_id, key_hash: $key_hash, clearance: $clearance, \
             service: $service, active: $active})",
            Params::new()
                .with("key_id", key_id.to_string())
                .with("key_hash", key_hash)
                .with("clearance", clearance.to_string())
                .with("service", service)
                .with("active", 1i64),
        )?;

        Ok(IssuedKey { key_id, secret })
    }

    /// Validate a presented secret, returning the service's [`FirmContext`].
    ///
    /// Fails with [`Error::Auth`] for an unknown, inactive, or mismatched key.
    pub fn validate(&self, secret: &str) -> Result<FirmContext> {
        let key_id = secret
            .split_once('.')
            .and_then(|(id, _)| Uuid::parse_str(id).ok())
            .ok_or_else(|| Error::Auth("malformed api key".into()))?;

        let row: KeyRow = self
            .store
            .query_one::<KeyRow>(
                "MATCH (a:ApiKey {key_id: $key_id}) RETURN a.key_hash, a.clearance, a.service, \
                 a.active",
                Params::new().with("key_id", key_id.to_string()),
            )?
            .ok_or_else(|| Error::Auth("unknown api key".into()))?;

        if row.active == 0 {
            return Err(Error::Auth("api key has been revoked".into()));
        }
        if !constant_time_eq(query_hash(secret).as_bytes(), row.key_hash.as_bytes()) {
            return Err(Error::Auth("api key secret mismatch".into()));
        }

        // Service identity: a bounded, non-human FirmContext.
        let user_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, key_id.as_bytes());
        Ok(
            FirmContext::new(user_id, format!("service:{}", row.service), row.clearance)
                .with_roles(["service".to_string()]),
        )
    }

    /// Revoke a key by flipping its `active` flag.
    pub fn revoke(&self, key_id: Uuid) -> Result<()> {
        self.store.execute_with(
            "MATCH (a:ApiKey {key_id: $key_id}) SET a.active = $active",
            Params::new()
                .with("key_id", key_id.to_string())
                .with("active", 0i64),
        )?;
        Ok(())
    }

    /// Rotate a key: revoke `key_id` and issue a fresh key for the same service
    /// and clearance. The old secret stops validating immediately.
    pub fn rotate(
        &self,
        key_id: Uuid,
        service: &str,
        clearance: ClearanceLevel,
    ) -> Result<IssuedKey> {
        self.revoke(key_id)?;
        self.issue(service, clearance)
    }
}

/// Generate a 256-bit random token, hex-encoded.
fn random_token() -> String {
    use argon2::password_hash::rand_core::{OsRng, RngCore};
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let mut s = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Constant-time byte-slice equality (avoids leaking match length via timing).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn issue_then_validate_yields_scoped_identity() {
        let (_d, store) = store();
        let svc = ApiKeyService::new(&store);
        let key = svc.issue("valuation-guest", ClearanceLevel::L3).unwrap();

        let ctx = svc.validate(&key.secret).unwrap();
        assert_eq!(ctx.clearance, ClearanceLevel::L3);
        assert_eq!(ctx.email, "service:valuation-guest");
        assert!(ctx.roles.contains(&"service".to_string()));
    }

    #[test]
    fn wrong_secret_is_rejected() {
        let (_d, store) = store();
        let svc = ApiKeyService::new(&store);
        let key = svc.issue("reporting-guest", ClearanceLevel::L2).unwrap();
        // Same key_id, tampered random part.
        let tampered = format!("{}.deadbeef", key.key_id);
        assert!(svc.validate(&tampered).is_err());
        assert!(svc.validate("garbage").is_err());
    }

    #[test]
    fn revoke_and_rotate_invalidate_the_old_secret() {
        let (_d, store) = store();
        let svc = ApiKeyService::new(&store);
        let key = svc.issue("compliance-guest", ClearanceLevel::L4).unwrap();
        assert!(svc.validate(&key.secret).is_ok());

        let new = svc
            .rotate(key.key_id, "compliance-guest", ClearanceLevel::L4)
            .unwrap();
        // Old secret no longer validates; new one does.
        assert!(svc.validate(&key.secret).is_err());
        let ctx = svc.validate(&new.secret).unwrap();
        assert_eq!(ctx.clearance, ClearanceLevel::L4);
    }
}
