//! # kanbrick-mesh
//!
//! Tachyon-Mesh runtime integration and WASM guest dispatch.
//!
//! Layer 2 (Nerves/Muscle) — wraps the vendored `crates/tachyon-mesh` submodule.
//!
//! Phase 0 scaffold: this crate compiles and exposes its public surface, but
//! the upstream integration is implemented in a later phase. See the GitHub
//! issue tracker for the phase that fills this in.

use kanbrick_core::Result;

/// Registry of loadable WASM guests. Phase 3 wires this to Tachyon-Mesh.
#[derive(Debug, Default)]
pub struct MeshRuntime {
    guests: Vec<String>,
}

impl MeshRuntime {
    /// Create an empty runtime.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a guest module by name.
    pub fn register(&mut self, name: impl Into<String>) -> Result<()> {
        self.guests.push(name.into());
        Ok(())
    }

    /// Names of registered guests.
    pub fn guests(&self) -> &[String] {
        &self.guests
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_tracks_guests() {
        let mut rt = MeshRuntime::new();
        rt.register("echo").unwrap();
        assert_eq!(rt.guests(), ["echo"]);
    }
}
