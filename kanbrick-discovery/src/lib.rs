//! # kanbrick-discovery
//!
//! graphify-rs graph analysis over the firm graph.
//!
//! Layer 4 (Map) — integrates the crates.io `graphify-rs` crate (edition 2024).
//!
//! Phase 0 scaffold: this crate compiles and exposes its public surface, but
//! the upstream integration is implemented in a later phase. See the GitHub
//! issue tracker for the phase that fills this in.

use kanbrick_core::Result;

/// Discovery engine over the firm graph. Phase 4 wires this to graphify-rs.
#[derive(Debug, Default)]
pub struct DiscoveryEngine;

impl DiscoveryEngine {
    /// Construct an empty engine.
    pub fn new() -> Self {
        DiscoveryEngine
    }

    /// Placeholder for shortest reporting-path queries (implemented in Phase 4).
    pub fn is_ready(&self) -> Result<bool> {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_not_ready_yet() {
        assert!(!DiscoveryEngine::new().is_ready().unwrap());
    }
}
