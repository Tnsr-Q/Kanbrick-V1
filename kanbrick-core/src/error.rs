//! Shared error type for the Kanbrick-V1 workspace.

use thiserror::Error;

/// Convenience alias for results that fail with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// The workspace-wide error enum. Layer crates wrap their own failures into
/// these variants so callers handle a single, stable error surface.
#[derive(Debug, Error)]
pub enum Error {
    /// The current [`crate::FirmContext`] lacks the required clearance.
    #[error("access denied: requires clearance {required}, have {actual}")]
    AccessDenied {
        /// Minimum clearance the operation requires.
        required: crate::ClearanceLevel,
        /// Clearance the caller actually holds.
        actual: crate::ClearanceLevel,
    },

    /// A requested entity was not found in the graph.
    #[error("not found: {0}")]
    NotFound(String),

    /// Input failed validation.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// A failure originating in the graph store layer.
    #[error("store error: {0}")]
    Store(String),

    /// A failure originating in the auth layer.
    #[error("auth error: {0}")]
    Auth(String),

    /// A catch-all for unexpected internal failures.
    #[error("internal error: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ClearanceLevel;

    #[test]
    fn access_denied_message() {
        let e = Error::AccessDenied {
            required: ClearanceLevel::L4,
            actual: ClearanceLevel::L2,
        };
        assert_eq!(
            e.to_string(),
            "access denied: requires clearance L4, have L2"
        );
    }
}
