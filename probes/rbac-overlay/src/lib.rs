//! P8.4 (#96) — Ironclaw RBAC + DLP as **additive-only overlays**.
//!
//! This is a throwaway de-risk spike (see `docs/adr/0010-ironclaw-rbac-dlp.md`).
//! Ironclaw ships as a **binary** (no library target — confirmed in Phase 2,
//! ADR-0002, where `kanbrick-auth` was built on Ironclaw's *primitives* rather
//! than the crate). So the de-risked outcome is not "depend on Ironclaw" but
//! "**port the RBAC/DLP pattern** into the firm OS as restrict-only overlays."
//! This spike proves that pattern in isolation.
//!
//! The two invariants the overlay must guarantee:
//!
//! 1. **Roles can only *restrict* clearance, never elevate it.** The production
//!    overlay reads the existing `kanbrick_core::FirmContext.roles`
//!    (`pub roles: Vec<String>`) — there is **no second role store**. A role maps
//!    to an optional clearance *ceiling*; the effective clearance is the base
//!    clearance lowered by every applicable ceiling (`min`), so adding a role can
//!    only ever narrow the result. There is deliberately no path by which a role
//!    raises clearance.
//!
//! 2. **DLP gates which provider a data class may be sent to**, default-deny.
//!    Used at the P9.6 provider boundary (ADR-0017): a (data-class, provider)
//!    pair must be explicitly allowlisted or the send is refused.
//!
//! `Clearance` here mirrors `kanbrick_core::ClearanceLevel` exactly (L1<..<L5,
//! derived `Ord`), so the logic transfers verbatim onto the real enum.

use std::collections::{HashMap, HashSet};

/// Mirror of `kanbrick_core::ClearanceLevel` (kanbrick-core/src/clearance.rs).
/// Declaration order is the privilege order, so `L1 < L2 < L3 < L4 < L5`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Clearance {
    L1,
    L2,
    L3,
    L4,
    L5,
}

/// A data sensitivity class attached to whatever a loop/skill wants to send to a
/// provider. (In production this is derived from the source `ProjectScope` +
/// graph labels; here it is an explicit tag.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataClass {
    /// Public roster / already-public company facts (ADR-0005).
    Public,
    /// Internal firm data.
    Internal,
    /// Restricted / sensitive (L4+ material, PII, deal terms).
    Restricted,
}

/// A BYO-AI provider egress target (host). Matches the per-tenant allowlist of
/// ADR-0017.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Anthropic,
    OpenAI,
    Cerebras,
}

/// The **restrict-only** RBAC overlay: role tags → an optional clearance ceiling.
///
/// Reads role tags exactly as they appear in `FirmContext.roles`. A role absent
/// from the map imposes no ceiling (it cannot grant anything). A role present
/// imposes a ceiling that can only *lower* the effective clearance.
#[derive(Debug, Default, Clone)]
pub struct RoleOverlay {
    ceilings: HashMap<String, Clearance>,
}

impl RoleOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a role that *caps* clearance at `ceiling` while it is held.
    /// e.g. `"contractor" -> L2`, `"dlp_quarantine" -> L1`.
    pub fn with_ceiling(mut self, role: impl Into<String>, ceiling: Clearance) -> Self {
        self.ceilings.insert(role.into(), ceiling);
        self
    }

    /// Effective clearance after applying every applicable role ceiling to the
    /// base clearance. **Monotonically non-increasing in the role set**: adding a
    /// role can only lower (or leave) the result — never raise it.
    pub fn effective_clearance(&self, base: Clearance, roles: &[String]) -> Clearance {
        let mut effective = base;
        for role in roles {
            if let Some(&ceiling) = self.ceilings.get(role) {
                // `min` is the whole safety argument: a ceiling can pull the
                // effective clearance down but the result is always `<= base`.
                effective = effective.min(ceiling);
            }
        }
        effective
    }
}

/// A DLP policy: the **default-deny** allowlist of (data-class, provider) pairs a
/// tenant may egress. Mirrors the per-tenant allowlist of ADR-0017; here it is a
/// flat set for the spike.
#[derive(Debug, Default, Clone)]
pub struct DlpPolicy {
    allowed: HashSet<(DataClass, Provider)>,
}

impl DlpPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow a specific (data-class → provider) egress pair.
    pub fn allow(mut self, class: DataClass, provider: Provider) -> Self {
        self.allowed.insert((class, provider));
        self
    }

    /// Default-deny: a pair is sendable only if explicitly allowlisted.
    pub fn can_send(&self, class: DataClass, provider: Provider) -> bool {
        self.allowed.contains(&(class, provider))
    }
}

/// Why an authorization attempt was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Denied {
    /// The caller's effective (post-overlay) clearance is below what the action
    /// requires.
    InsufficientClearance {
        required: Clearance,
        effective: Clearance,
    },
    /// DLP refused this (data-class → provider) egress.
    DlpBlocked {
        class: DataClass,
        provider: Provider,
    },
}

/// The combined additive-only gate, in the order the provider boundary applies
/// it: first narrow clearance by roles, check the action's minimum, then apply
/// DLP to the egress pair. Both checks must pass.
pub fn authorize_send(
    overlay: &RoleOverlay,
    dlp: &DlpPolicy,
    base_clearance: Clearance,
    roles: &[String],
    required: Clearance,
    class: DataClass,
    provider: Provider,
) -> Result<Clearance, Denied> {
    let effective = overlay.effective_clearance(base_clearance, roles);
    if effective < required {
        return Err(Denied::InsufficientClearance {
            required,
            effective,
        });
    }
    if !dlp.can_send(class, provider) {
        return Err(Denied::DlpBlocked { class, provider });
    }
    Ok(effective)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles(rs: &[&str]) -> Vec<String> {
        rs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn clearance_order_matches_kanbrick_core() {
        // Same ordering invariant the real ClearanceLevel relies on.
        assert!(Clearance::L1 < Clearance::L5);
        assert!(Clearance::L3 < Clearance::L4);
    }

    #[test]
    fn role_restricts_clearance() {
        let overlay = RoleOverlay::new().with_ceiling("contractor", Clearance::L2);
        // An L4 strategist who is also tagged `contractor` is capped at L2.
        let eff = overlay.effective_clearance(Clearance::L4, &roles(&["contractor"]));
        assert_eq!(eff, Clearance::L2);
    }

    #[test]
    fn role_can_never_elevate() {
        // Even a role whose *ceiling* is L5 cannot raise an L2 caller. `min`
        // guarantees the result is `<= base` for every possible role set.
        let overlay = RoleOverlay::new()
            .with_ceiling("admin", Clearance::L5)
            .with_ceiling("contractor", Clearance::L1);
        assert_eq!(
            overlay.effective_clearance(Clearance::L2, &roles(&["admin"])),
            Clearance::L2,
            "a high-ceiling role must not elevate"
        );
        // Adding more roles is monotonically non-increasing.
        assert_eq!(
            overlay.effective_clearance(Clearance::L2, &roles(&["admin", "contractor"])),
            Clearance::L1,
            "adding a restrictive role lowers further"
        );
        // Unknown roles impose no ceiling (and certainly cannot elevate).
        assert_eq!(
            overlay.effective_clearance(Clearance::L3, &roles(&["analyst", "visitor"])),
            Clearance::L3
        );
    }

    #[test]
    fn restricted_call_denied_allowed_passes() {
        let overlay = RoleOverlay::new().with_ceiling("contractor", Clearance::L2);
        let dlp = DlpPolicy::new().allow(DataClass::Public, Provider::Anthropic);

        // Capped to L2 by the role, an L3-required action is denied...
        let denied = authorize_send(
            &overlay,
            &dlp,
            Clearance::L4,
            &roles(&["contractor"]),
            Clearance::L3,
            DataClass::Public,
            Provider::Anthropic,
        );
        assert_eq!(
            denied,
            Err(Denied::InsufficientClearance {
                required: Clearance::L3,
                effective: Clearance::L2,
            })
        );

        // ...but the same caller without the restricting role passes the gate.
        let ok = authorize_send(
            &overlay,
            &dlp,
            Clearance::L4,
            &roles(&[]),
            Clearance::L3,
            DataClass::Public,
            Provider::Anthropic,
        );
        assert_eq!(ok, Ok(Clearance::L4));
    }

    #[test]
    fn dlp_blocks_disallowed_pair_allows_allowed() {
        let overlay = RoleOverlay::new();
        let dlp = DlpPolicy::new()
            .allow(DataClass::Public, Provider::Anthropic)
            .allow(DataClass::Internal, Provider::Anthropic);

        // Allowed pair → passes.
        assert!(authorize_send(
            &overlay,
            &dlp,
            Clearance::L4,
            &roles(&[]),
            Clearance::L1,
            DataClass::Public,
            Provider::Anthropic,
        )
        .is_ok());

        // Restricted data → Anthropic is NOT allowlisted → DLP blocks it, even
        // for an L5 caller (DLP is orthogonal to clearance).
        assert_eq!(
            authorize_send(
                &overlay,
                &dlp,
                Clearance::L5,
                &roles(&[]),
                Clearance::L1,
                DataClass::Restricted,
                Provider::Anthropic,
            ),
            Err(Denied::DlpBlocked {
                class: DataClass::Restricted,
                provider: Provider::Anthropic,
            })
        );
    }

    #[test]
    fn dlp_is_default_deny() {
        let dlp = DlpPolicy::new().allow(DataClass::Public, Provider::Anthropic);
        // A pair that was never allowlisted is refused.
        assert!(!dlp.can_send(DataClass::Public, Provider::OpenAI));
        assert!(!dlp.can_send(DataClass::Internal, Provider::Cerebras));
        // The one explicitly allowed pair is permitted.
        assert!(dlp.can_send(DataClass::Public, Provider::Anthropic));
    }
}
