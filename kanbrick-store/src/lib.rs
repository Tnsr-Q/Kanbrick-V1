//! # kanbrick-store
//!
//! Embedded SparrowDB lifecycle wrapper, firm schema, parameterized queries,
//! and versioned migrations.
//!
//! Layer 3 (Brain) — wraps the vendored `crates/sparrowdb` submodule.
//!
//! ## Surface
//!
//! * [`Store`] — open/close lifecycle and the query surface (issues #6, #9).
//! * [`schema`] — typed `PersonNode`/`CompanyNode`/`SegmentNode` and schema DDL
//!   (issue #8).
//! * [`value::Params`] — injection-safe bound query parameters (issue #9).
//! * [`migrations::Migrator`] — versioned schema & seed migrations (issue #10).
//! * [`seed`] — Cypher seed-file loading (issue #11).

pub mod migrations;
pub mod schema;
pub mod seed;
pub mod store;
pub mod value;

pub use migrations::{Migration, MigrationOutcome, Migrator};
pub use schema::{CompanyNode, PersonNode, SegmentNode};
pub use store::Store;
pub use value::Params;

// Re-export the SparrowDB result type consumers need when reading raw query
// results without going through typed deserialization.
pub use sparrowdb::QueryResult;
