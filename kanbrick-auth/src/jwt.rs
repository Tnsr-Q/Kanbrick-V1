//! JWT issuance & validation, and the claims ⇄ [`FirmContext`] mapping
//! (issues #13, #14).
//!
//! Tokens are signed with HS256 over a [`Claims`] payload that mirrors the
//! [`FirmContext`] identity. Issuance stamps `iat`/`exp` from a configurable
//! TTL; validation checks the signature and expiry and rehydrates a
//! `FirmContext`. Any malformed, tampered, or expired token yields
//! [`Error::Auth`] (never a panic).

use chrono::{Duration, TimeZone, Utc};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use kanbrick_core::{ClearanceLevel, Error, FirmContext, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// JWT claims payload. Field names follow JWT conventions (`sub`, `iat`, `exp`)
/// where they exist, plus the firm-specific identity fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Subject — the authenticated person's `user_id` (UUID).
    pub sub: String,
    /// Login email.
    pub email: String,
    /// Effective clearance for the session.
    pub clearance: ClearanceLevel,
    /// Owning firm id.
    pub firm_id: String,
    /// Coarse role tags.
    pub roles: Vec<String>,
    /// Session id (UUID).
    pub sid: String,
    /// Issued-at (Unix seconds).
    pub iat: i64,
    /// Expiry (Unix seconds).
    pub exp: i64,
}

impl Claims {
    /// Build claims from a [`FirmContext`], expiring `ttl` after its
    /// `issued_at`.
    pub fn from_context(ctx: &FirmContext, ttl: Duration) -> Self {
        let iat = ctx.issued_at.timestamp();
        Claims {
            sub: ctx.user_id.to_string(),
            email: ctx.email.clone(),
            clearance: ctx.clearance,
            firm_id: ctx.firm_id.clone(),
            roles: ctx.roles.clone(),
            sid: ctx.session_id.to_string(),
            iat,
            exp: iat + ttl.num_seconds(),
        }
    }

    /// Rehydrate a [`FirmContext`] from validated claims.
    pub fn into_context(self) -> Result<FirmContext> {
        let user_id = Uuid::parse_str(&self.sub)
            .map_err(|_| Error::Auth("token subject is not a valid id".into()))?;
        let session_id = Uuid::parse_str(&self.sid)
            .map_err(|_| Error::Auth("token session id is invalid".into()))?;
        let issued_at = Utc
            .timestamp_opt(self.iat, 0)
            .single()
            .ok_or_else(|| Error::Auth("token issued-at is out of range".into()))?;
        Ok(FirmContext {
            user_id,
            email: self.email,
            clearance: self.clearance,
            firm_id: self.firm_id,
            roles: self.roles,
            session_id,
            issued_at,
        })
    }
}

/// Issues and validates firm JWTs with a shared HS256 secret.
pub struct JwtAuthenticator {
    encoding: EncodingKey,
    decoding: DecodingKey,
    validation: Validation,
    ttl: Duration,
}

impl JwtAuthenticator {
    /// Create an authenticator from a signing `secret` and session `ttl`.
    pub fn new(secret: &[u8], ttl: Duration) -> Self {
        let mut validation = Validation::new(Algorithm::HS256);
        // Expiry is validated via the `exp` claim with no clock leeway (the
        // default 60s grace would let just-expired tokens through).
        validation.validate_exp = true;
        validation.leeway = 0;
        JwtAuthenticator {
            encoding: EncodingKey::from_secret(secret),
            decoding: DecodingKey::from_secret(secret),
            validation,
            ttl,
        }
    }

    /// The session lifetime applied at issuance.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Issue a signed JWT for `ctx`.
    pub fn issue(&self, ctx: &FirmContext) -> Result<String> {
        let claims = Claims::from_context(ctx, self.ttl);
        encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)
            .map_err(|e| Error::Auth(format!("failed to sign token: {e}")))
    }

    /// Validate a JWT and return the embedded [`FirmContext`].
    ///
    /// Returns [`Error::Auth`] for tampered signatures, malformed tokens, or
    /// expired sessions.
    pub fn validate(&self, token: &str) -> Result<FirmContext> {
        let data = decode::<Claims>(token, &self.decoding, &self.validation).map_err(|e| {
            use jsonwebtoken::errors::ErrorKind;
            match e.kind() {
                ErrorKind::ExpiredSignature => Error::Auth("token has expired".into()),
                ErrorKind::InvalidSignature => Error::Auth("token signature is invalid".into()),
                _ => Error::Auth(format!("invalid token: {e}")),
            }
        })?;
        data.claims.into_context()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> FirmContext {
        FirmContext::new(
            Uuid::new_v4(),
            "tracy.brittcool@kanbrick.com",
            ClearanceLevel::L5,
        )
        .with_roles(["admin".to_string()])
    }

    #[test]
    fn issue_then_validate_round_trips() {
        let auth = JwtAuthenticator::new(b"test-secret", Duration::hours(1));
        let original = ctx();
        let token = auth.issue(&original).unwrap();
        let back = auth.validate(&token).unwrap();
        assert_eq!(back.user_id, original.user_id);
        assert_eq!(back.email, original.email);
        assert_eq!(back.clearance, ClearanceLevel::L5);
        assert_eq!(back.roles, vec!["admin".to_string()]);
        assert_eq!(back.session_id, original.session_id);
    }

    #[test]
    fn tampered_signature_is_rejected() {
        let auth = JwtAuthenticator::new(b"test-secret", Duration::hours(1));
        let token = auth.issue(&ctx()).unwrap();
        let mut bytes = token.into_bytes();
        *bytes.last_mut().unwrap() ^= 0x01; // flip a bit in the signature
        let tampered = String::from_utf8(bytes).unwrap();
        assert!(auth.validate(&tampered).is_err());
    }

    #[test]
    fn wrong_secret_is_rejected() {
        let issuer = JwtAuthenticator::new(b"secret-a", Duration::hours(1));
        let verifier = JwtAuthenticator::new(b"secret-b", Duration::hours(1));
        let token = issuer.issue(&ctx()).unwrap();
        assert!(verifier.validate(&token).is_err());
    }

    #[test]
    fn expired_token_is_rejected() {
        let auth = JwtAuthenticator::new(b"test-secret", Duration::seconds(-10));
        // Negative TTL => exp is already in the past.
        let token = auth.issue(&ctx()).unwrap();
        let err = auth.validate(&token).unwrap_err();
        assert!(matches!(err, Error::Auth(_)));
    }

    #[test]
    fn garbage_is_rejected_without_panic() {
        let auth = JwtAuthenticator::new(b"test-secret", Duration::hours(1));
        assert!(auth.validate("not.a.jwt").is_err());
        assert!(auth.validate("").is_err());
        assert!(auth.validate("aaaa.bbbb.cccc").is_err());
    }
}
