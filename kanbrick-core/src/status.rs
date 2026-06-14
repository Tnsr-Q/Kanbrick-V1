//! Lifecycle status shared by firm entities.

use serde::{Deserialize, Serialize};

/// Whether an entity (person or company) is currently active.
///
/// Serializes lowercase (`"active"` / `"inactive"`) to match the property
/// values used in the firm seed data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// The entity is in active service.
    Active,
    /// The entity has been retired or deactivated.
    Inactive,
}

impl Status {
    /// Returns `true` for [`Status::Active`].
    pub fn is_active(self) -> bool {
        matches!(self, Status::Active)
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Status::Active => "active",
            Status::Inactive => "inactive",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&Status::Active).unwrap(),
            "\"active\""
        );
        let back: Status = serde_json::from_str("\"inactive\"").unwrap();
        assert_eq!(back, Status::Inactive);
        assert!(Status::Active.is_active());
    }
}
