//! # kanbrick-core
//!
//! Foundational types shared across every Kanbrick-V1 layer. This crate has no
//! dependency on the vendored upstreams (SparrowDB, Ironclaw, Tachyon-Mesh,
//! graphify-rs); it defines the vocabulary they are all wired against:
//!
//! * [`ClearanceLevel`] — the firm's five-tier access model (L1..L5).
//! * [`FirmContext`] — the security identity propagated on every request.
//! * [`NodeLabel`] / [`EdgeLabel`] — the graph schema vocabulary.
//! * [`PersonId`] / [`CompanyId`] / [`SegmentCode`] — typed identifiers.
//! * [`Status`] — entity lifecycle status.
//! * [`Error`] / [`ErrorKind`] / [`Result`] — the shared error types.
//! * [`abi`] — the host↔guest WASM ABI (traits + JSON DTOs) shared by the mesh
//!   runtime and guests.

pub mod abi;
pub mod clearance;
pub mod context;
pub mod error;
pub mod ids;
pub mod schema;
pub mod status;

pub use clearance::ClearanceLevel;
pub use context::FirmContext;
pub use error::{Error, ErrorKind, Result};
pub use ids::{CompanyId, PersonId, SegmentCode};
pub use schema::{EdgeLabel, NodeLabel};
pub use status::Status;

/// The firm identifier used throughout Kanbrick-V1. Always `"kanbrick"` for V1.
pub const FIRM_ID: &str = "kanbrick";
