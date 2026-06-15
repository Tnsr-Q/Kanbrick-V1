//! Clearance enforcement gate (issue #16).
//!
//! [`require_clearance`] is the imperative gate used at call sites and behind the
//! API middleware: it admits a request only when the caller's [`FirmContext`]
//! meets a required minimum [`ClearanceLevel`], and otherwise returns the
//! structured [`Error::AccessDenied`].
//!
//! Row- and field-level *data* scoping (the L1–L5 visibility model) lives in
//! [`crate::scope`]; this module is the coarse "are you cleared to call this at
//! all" check.

use kanbrick_core::{ClearanceLevel, Error, FirmContext, Result};

/// Admit the request only if `ctx` satisfies the `required` minimum clearance.
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
        assert!(require_clearance(&ctx, ClearanceLevel::L3).is_ok());
        assert!(require_clearance(&ctx, ClearanceLevel::L5).is_err());
    }
}
