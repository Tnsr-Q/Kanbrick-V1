//! # kanbrick-store
//!
//! Embedded SparrowDB lifecycle wrapper and schema migrations.
//!
//! Layer 3 (Brain) — wraps the vendored `crates/sparrowdb` submodule.
//!
//! Phase 0 scaffold: this crate compiles and exposes its public surface, but
//! the upstream integration is implemented in a later phase. See the GitHub
//! issue tracker for the phase that fills this in.

use kanbrick_core::Result;

/// Handle to the embedded graph store. Phase 1 wires this to SparrowDB.
#[derive(Debug, Default)]
pub struct Store {
    path: std::path::PathBuf,
}

impl Store {
    /// Open (or prepare to open) a store rooted at `path`.
    pub fn open(path: impl Into<std::path::PathBuf>) -> Result<Self> {
        Ok(Store { path: path.into() })
    }

    /// Filesystem location backing this store.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_records_path() {
        let s = Store::open("/tmp/firm.db").unwrap();
        assert_eq!(s.path().to_str(), Some("/tmp/firm.db"));
    }
}
