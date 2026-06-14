//! Login flow: email + password → JWT carrying the person's clearance
//! (issue #15).
//!
//! Credentials live as a hashed `password_hash` property on the `Person` node
//! (PRD 2.5). [`LoginService::set_password`] provisions one; [`LoginService::login`]
//! verifies it and issues a signed JWT whose claims carry the person's effective
//! clearance. A missing person, a person without credentials, and a bad password
//! all fail identically as [`Error::Auth`] so the flow does not leak which case
//! occurred.

use kanbrick_core::{ClearanceLevel, Error, FirmContext, Result};
use kanbrick_store::{Params, Store};
use serde::Deserialize;
use uuid::Uuid;

use crate::jwt::JwtAuthenticator;
use crate::password;

/// Fixed namespace for deriving a stable per-person `user_id` from email, so the
/// JWT subject is deterministic without adding an id column to the seed.
const FIRM_NAMESPACE: Uuid = Uuid::from_bytes([
    0x6b, 0x61, 0x6e, 0x62, 0x72, 0x69, 0x63, 0x6b, 0x2d, 0x76, 0x31, 0x00, 0x00, 0x00, 0x00, 0x01,
]);

/// Derive the stable `user_id` for a person from their email.
pub fn user_id_for(email: &str) -> Uuid {
    Uuid::new_v5(&FIRM_NAMESPACE, email.as_bytes())
}

/// The credential + identity fields read during login.
///
/// Field names match the `Person` property names exactly: SparrowDB returns a
/// `Null` cell when a projection is aliased to a *different* name on a bare-node
/// match (see ADR-0001), so the lookup query projects properties un-aliased and
/// the store's row mapper strips the `p.` prefix onto these field names.
#[derive(Debug, Deserialize)]
struct CredentialRow {
    clearance_level: ClearanceLevel,
    #[serde(default)]
    password_hash: Option<String>,
    #[serde(default)]
    role: Option<String>,
}

/// Authenticates persons against stored credentials and issues JWTs.
pub struct LoginService<'a> {
    store: &'a Store,
    jwt: &'a JwtAuthenticator,
}

impl<'a> LoginService<'a> {
    /// Build a login service over a store and a JWT authenticator.
    pub fn new(store: &'a Store, jwt: &'a JwtAuthenticator) -> Self {
        LoginService { store, jwt }
    }

    /// Provision (or rotate) a person's password, storing only the Argon2id
    /// hash on their `Person` node.
    pub fn set_password(&self, email: &str, password: &str) -> Result<()> {
        // The person must already exist; SET on a non-matching MATCH is a no-op,
        // so confirm a row was actually updated.
        let hash = password::hash_password(password)?;
        self.store.execute_with(
            "MATCH (p:Person {email: $email}) SET p.password_hash = $hash",
            Params::new().with("email", email).with("hash", hash),
        )?;
        if self.lookup(email)?.is_none() {
            return Err(Error::NotFound(format!("no person with email {email}")));
        }
        Ok(())
    }

    /// Authenticate `email`/`password`; on success return a signed JWT.
    pub fn login(&self, email: &str, password: &str) -> Result<String> {
        let row = self
            .lookup(email)?
            .ok_or_else(|| Error::Auth("invalid email or password".into()))?;

        let hash = row
            .password_hash
            .as_deref()
            .ok_or_else(|| Error::Auth("invalid email or password".into()))?;

        if !password::verify_password(password, hash)? {
            return Err(Error::Auth("invalid email or password".into()));
        }

        let mut ctx = FirmContext::new(user_id_for(email), email, row.clearance_level);
        if let Some(role) = row.role {
            ctx = ctx.with_roles([role]);
        }
        self.jwt.issue(&ctx)
    }

    /// Read a person's identity + credential fields by email.
    fn lookup(&self, email: &str) -> Result<Option<CredentialRow>> {
        self.store.query_one::<CredentialRow>(
            "MATCH (p:Person {email: $email}) RETURN \
             p.clearance_level, p.password_hash, p.role",
            Params::new().with("email", email),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
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

    #[test]
    fn login_returns_jwt_with_correct_clearance() {
        let (_d, store) = seeded();
        let jwt = JwtAuthenticator::new(b"secret", Duration::hours(1));
        let svc = LoginService::new(&store, &jwt);

        svc.set_password("tracy.brittcool@kanbrick.com", "ceo-pw")
            .unwrap();
        let token = svc.login("tracy.brittcool@kanbrick.com", "ceo-pw").unwrap();

        let ctx = jwt.validate(&token).unwrap();
        assert_eq!(ctx.clearance, ClearanceLevel::L5);
        assert_eq!(ctx.email, "tracy.brittcool@kanbrick.com");
        assert_eq!(ctx.user_id, user_id_for("tracy.brittcool@kanbrick.com"));
    }

    #[test]
    fn wrong_password_is_rejected() {
        let (_d, store) = seeded();
        let jwt = JwtAuthenticator::new(b"secret", Duration::hours(1));
        let svc = LoginService::new(&store, &jwt);
        svc.set_password("elena.ruiz@kanbrick.com", "right")
            .unwrap();
        assert!(svc.login("elena.ruiz@kanbrick.com", "wrong").is_err());
    }

    #[test]
    fn unknown_email_and_uncredentialed_person_both_fail() {
        let (_d, store) = seeded();
        let jwt = JwtAuthenticator::new(b"secret", Duration::hours(1));
        let svc = LoginService::new(&store, &jwt);
        // Never provisioned.
        assert!(svc.login("nobody@kanbrick.com", "x").is_err());
        assert!(svc.login("dana.prescott@kanbrick.com", "x").is_err());
    }

    #[test]
    fn set_password_on_unknown_email_errors() {
        let (_d, store) = seeded();
        let jwt = JwtAuthenticator::new(b"secret", Duration::hours(1));
        let svc = LoginService::new(&store, &jwt);
        assert!(svc.set_password("ghost@kanbrick.com", "x").is_err());
    }
}
