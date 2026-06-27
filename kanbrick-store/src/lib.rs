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
//! * [`guest_policy`] — persisted per-guest version/clearance/asset policy (#64).
//! * [`registry_meta`] — persisted registry generation counter (#69).
//! * [`messenger_log`] — durable, append-only `(:MessengerMessage)` history (#114).

pub mod guest_policy;
pub mod messenger_log;
pub mod migrations;
pub mod registry_meta;
pub mod schema;
pub mod seed;
pub mod store;
pub mod value;

pub use guest_policy::{
    list_guest_policies, read_guest_policy, write_guest_policy, GuestPolicy, SOURCE_EMBEDDED,
    SOURCE_REGISTRY,
};
pub use messenger_log::{count_messages, list_messages, persist_message};
pub use migrations::{Migration, MigrationOutcome, Migrator};
pub use registry_meta::{bump_registry_generation, read_registry_generation};
pub use schema::{CompanyNode, PersonNode, SegmentNode};
pub use store::Store;
pub use value::Params;

// Re-export the SparrowDB result type consumers need when reading raw query
// results without going through typed deserialization.
pub use sparrowdb::QueryResult;
