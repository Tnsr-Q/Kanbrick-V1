//! Strongly-typed identifier newtypes.
//!
//! The firm graph keys persons by email, companies by their short code, and
//! segments by their code. Wrapping these in distinct newtypes stops them from
//! being swapped for one another (a `CompanyId` can never be passed where a
//! `PersonId` is expected) while still serializing as plain strings.

use serde::{Deserialize, Serialize};

/// Macro to declare a `String`-backed identifier newtype.
///
/// Each generated type is `#[serde(transparent)]`, so it round-trips through
/// JSON as a bare string with no wrapper object.
macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wrap an existing string as this identifier.
            pub fn new(value: impl Into<String>) -> Self {
                $name(value.into())
            }

            /// Borrow the underlying string.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume the newtype and return the inner `String`.
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                $name(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                $name(value.to_string())
            }
        }
    };
}

string_id! {
    /// Identifies a [`Person`](crate::NodeLabel::Person) by their email handle.
    PersonId
}

string_id! {
    /// Identifies a [`Company`](crate::NodeLabel::Company) by its short code
    /// (e.g. `"JMTS"`).
    CompanyId
}

string_id! {
    /// Identifies a [`Segment`](crate::NodeLabel::Segment) by its code
    /// (e.g. `"TLS"`).
    SegmentCode
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construction_and_display() {
        let p = PersonId::from("tracy.brittcool@kanbrick.com");
        assert_eq!(p.as_str(), "tracy.brittcool@kanbrick.com");
        assert_eq!(p.to_string(), "tracy.brittcool@kanbrick.com");
        assert_eq!(CompanyId::new("JMTS").into_inner(), "JMTS");
    }

    #[test]
    fn json_round_trips_as_bare_string() {
        let c = CompanyId::from("JMTS");
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "\"JMTS\"");
        let back: CompanyId = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);

        let s = SegmentCode::from("TLS");
        let back: SegmentCode = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s, back);
    }
}
