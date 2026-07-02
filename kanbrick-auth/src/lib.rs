//! # kanbrick-auth
//!
//! Identity & auth for Kanbrick-V1 — Layer 1 (Face/Guard).
//!
//! Ironclaw ships as a binary crate (no library target), so rather than
//! depending on it we build the firm's auth on the same primitives it uses —
//! `jsonwebtoken` (JWT) and `argon2` (Argon2id) — which is the PRD's stated
//! mitigation for integrating Ironclaw's security model.
//!
//! ## Surface
//!
//! * [`jwt`] — JWT issuance/validation and the claims ⇄ `FirmContext` mapping
//!   (issues #13, #14).
//! * [`password`] — Argon2id password hashing (PRD 2.5).
//! * [`login`] — email + password → JWT login flow (issue #15).
//! * [`clearance`] — the `require_clearance` gate (issue #16).
//! * [`scope`] — clearance-scoped data visibility (issue #17).
//! * [`guarded`] — the audited, clearance-filtering query interceptor (issue #18),
//!   read-only toward guests (P16.1, ADR-0022).
//! * [`audit`] — per-query audit logging (issue #19).
//! * [`apikey`] — scoped, rotatable service API keys (issue #20).

pub mod apikey;
pub mod audit;
pub mod clearance;
pub mod guarded;
pub mod jwt;
pub mod login;
pub mod password;
mod readonly;
pub mod scope;

pub use apikey::{ApiKeyService, IssuedKey};
pub use audit::AuditLog;
pub use clearance::require_clearance;
pub use guarded::GuardedStore;
pub use jwt::{Claims, JwtAuthenticator};
pub use login::LoginService;
pub use scope::ClearanceScope;
