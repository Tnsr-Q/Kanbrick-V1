//! Argon2id password hashing and verification (PRD 2.5).
//!
//! Credentials are stored as the PHC-string form of an Argon2id hash (salt and
//! parameters embedded), never as plaintext. [`hash_password`] mints a fresh
//! random salt per password; [`verify_password`] is constant-time via the
//! underlying `password_hash` comparison.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use kanbrick_core::{Error, Result};

/// Hash a plaintext password with Argon2id, returning a PHC string suitable for
/// storage as a `Person.password_hash` property.
pub fn hash_password(plaintext: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(plaintext.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| Error::Internal(format!("password hashing failed: {e}")))
}

/// Verify a plaintext password against a stored PHC hash string.
///
/// Returns `Ok(true)` on a match, `Ok(false)` on a mismatch, and
/// [`Error::Internal`] only if the stored hash itself is malformed.
pub fn verify_password(plaintext: &str, phc_hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(phc_hash)
        .map_err(|e| Error::Internal(format!("stored password hash is malformed: {e}")))?;
    Ok(Argon2::default()
        .verify_password(plaintext.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_verifies_against_itself() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &hash).unwrap());
        assert!(!verify_password("wrong password", &hash).unwrap());
    }

    #[test]
    fn same_password_gets_distinct_salts() {
        let a = hash_password("hunter2").unwrap();
        let b = hash_password("hunter2").unwrap();
        assert_ne!(a, b, "each hash must use a fresh random salt");
        assert!(verify_password("hunter2", &a).unwrap());
        assert!(verify_password("hunter2", &b).unwrap());
    }

    #[test]
    fn malformed_stored_hash_errors() {
        assert!(verify_password("x", "not-a-phc-string").is_err());
    }
}
