//! # kanbrick-auth
//!
//! Ironclaw integration: JWT issuance/validation and clearance enforcement.
//!
//! Layer 1 (Face/Guard) — wraps the vendored `crates/ironclaw` submodule.
//!
//! Phase 0 scaffold: this crate compiles and exposes its public surface, but
//! the upstream integration is implemented in a later phase. See the GitHub
//! issue tracker for the phase that fills this in.

use kanbrick_core::Result;

use kanbrick_core::{ClearanceLevel, Error, FirmContext};

/// Gate helper: succeeds only if `ctx` meets the `required` clearance.
/// Phase 2 expands this into full middleware backed by Ironclaw.
pub fn require_clearance(ctx: &FirmContext, required: ClearanceLevel) -> Result<()> {
    if ctx.has_clearance(required) {
        Ok(())
    } else {
        Err(Error::AccessDenied {
            required,
            actual: ctx.clearance,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn gate_allows_and_denies() {
        let ctx = FirmContext::new(Uuid::nil(), "a@kanbrick.com", ClearanceLevel::L3);
        assert!(require_clearance(&ctx, ClearanceLevel::L2).is_ok());
        assert!(require_clearance(&ctx, ClearanceLevel::L5).is_err());
    }
}
