//! Shared error type for the Kanbrick-V1 workspace.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Convenience alias for results that fail with [`enum@Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Coarse classification of an [`enum@Error`].
///
/// Where [`enum@Error`] carries the specific failure (with its message), `ErrorKind`
/// is the stable category callers branch on — e.g. to map onto an HTTP status
/// in the API layer — without matching every concrete variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorKind {
    /// The caller is not authorized (insufficient clearance or bad credentials).
    Unauthorized,
    /// A requested entity does not exist.
    NotFound,
    /// A graph query failed to parse, bind, or execute.
    QueryError,
    /// Caller-supplied input failed validation.
    ValidationError,
    /// An unexpected internal failure.
    Internal,
}

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

    /// A graph query failed to parse, bind, or execute.
    #[error("query error: {0}")]
    Query(String),

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

impl Error {
    /// Classify this error into its coarse [`ErrorKind`] category.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Error::AccessDenied { .. } => ErrorKind::Unauthorized,
            Error::Auth(_) => ErrorKind::Unauthorized,
            Error::NotFound(_) => ErrorKind::NotFound,
            Error::InvalidInput(_) => ErrorKind::ValidationError,
            Error::Query(_) | Error::Store(_) => ErrorKind::QueryError,
            Error::Internal(_) => ErrorKind::Internal,
        }
    }
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

    #[test]
    fn kinds_map_to_categories() {
        assert_eq!(
            Error::AccessDenied {
                required: ClearanceLevel::L4,
                actual: ClearanceLevel::L2,
            }
            .kind(),
            ErrorKind::Unauthorized
        );
        assert_eq!(Error::NotFound("x".into()).kind(), ErrorKind::NotFound);
        assert_eq!(Error::Query("bad".into()).kind(), ErrorKind::QueryError);
        assert_eq!(
            Error::InvalidInput("y".into()).kind(),
            ErrorKind::ValidationError
        );
    }

    #[test]
    fn error_kind_round_trips() {
        let json = serde_json::to_string(&ErrorKind::QueryError).unwrap();
        let back: ErrorKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ErrorKind::QueryError);
    }
}
