//! # kanbrick-core
//!
//! Foundational types shared across every Kanbrick-V1 layer. This crate has no
//! dependency on the vendored upstreams (SparrowDB, Ironclaw, Tachyon-Mesh,
//! graphify-rs); it defines the vocabulary they are all wired against:
//!
//! * [`ClearanceLevel`] — the firm's five-tier access model (L1..L5).
//! * [`FirmContext`] — the security identity propagated on every request.
//! * [`NodeLabel`] / [`EdgeLabel`] — the graph schema vocabulary.
//! * [`Error`] / [`Result`] — the shared error type.

pub mod clearance;
pub mod context;
pub mod error;
pub mod schema;

pub use clearance::ClearanceLevel;
pub use context::FirmContext;
pub use error::{Error, Result};
pub use schema::{EdgeLabel, NodeLabel};

/// The firm identifier used throughout Kanbrick-V1. Always `"kanbrick"` for V1.
pub const FIRM_ID: &str = "kanbrick";
