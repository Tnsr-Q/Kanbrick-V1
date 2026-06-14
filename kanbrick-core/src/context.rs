//! [`FirmContext`] — the security identity propagated through every layer.

use crate::clearance::ClearanceLevel;
use crate::FIRM_ID;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identity and authorization context carried on every request.
///
/// Created by the auth layer (Phase 2) after a successful login, serialized
/// into JWT claims, and rehydrated on each request. It then flows unchanged
/// through the mesh into WASM guests, where it constrains every graph query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmContext {
    /// Stable identifier for the authenticated person.
    pub user_id: Uuid,
    /// The person's email (also their login handle).
    pub email: String,
    /// Effective clearance for this session.
    pub clearance: ClearanceLevel,
    /// Owning firm. Always [`FIRM_ID`] for V1.
    pub firm_id: String,
    /// Coarse-grained role tags (e.g. `"segment_lead"`, `"analyst"`).
    pub roles: Vec<String>,
    /// Identifier for the active session.
    pub session_id: Uuid,
    /// When the session was issued.
    pub issued_at: DateTime<Utc>,
}

impl FirmContext {
    /// Build a context for the default firm with a freshly minted session id.
    pub fn new(user_id: Uuid, email: impl Into<String>, clearance: ClearanceLevel) -> Self {
        FirmContext {
            user_id,
            email: email.into(),
            clearance,
            firm_id: FIRM_ID.to_string(),
            roles: Vec::new(),
            session_id: Uuid::new_v4(),
            issued_at: Utc::now(),
        }
    }

    /// Attach role tags, returning the updated context (builder style).
    pub fn with_roles(mut self, roles: impl IntoIterator<Item = String>) -> Self {
        self.roles = roles.into_iter().collect();
        self
    }

    /// Whether this context meets a required minimum clearance.
    pub fn has_clearance(&self, required: ClearanceLevel) -> bool {
        self.clearance.satisfies(required)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_context_uses_default_firm() {
        let ctx = FirmContext::new(Uuid::new_v4(), "a@kanbrick.com", ClearanceLevel::L3);
        assert_eq!(ctx.firm_id, FIRM_ID);
        assert!(ctx.has_clearance(ClearanceLevel::L2));
        assert!(!ctx.has_clearance(ClearanceLevel::L4));
    }

    #[test]
    fn json_round_trip() {
        let ctx = FirmContext::new(Uuid::nil(), "b@kanbrick.com", ClearanceLevel::L5)
            .with_roles(["admin".to_string()]);
        let json = serde_json::to_string(&ctx).unwrap();
        let back: FirmContext = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx, back);
        assert_eq!(back.roles, vec!["admin".to_string()]);
    }
}
