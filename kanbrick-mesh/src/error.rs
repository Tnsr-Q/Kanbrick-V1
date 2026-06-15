//! Error type for the mesh runtime.

use thiserror::Error;

/// Result alias for mesh operations.
pub type Result<T> = std::result::Result<T, MeshError>;

/// A failure in the WASM runtime: engine setup, module loading, or dispatch.
#[derive(Debug, Error)]
pub enum MeshError {
    /// The wasmtime engine could not be created from the runtime config.
    #[error("failed to create wasm engine: {0}")]
    Engine(String),

    /// A guest module failed to compile.
    #[error("failed to compile guest module {name:?}: {detail}")]
    Compile {
        /// The guest's registered name.
        name: String,
        /// The underlying compiler error.
        detail: String,
    },

    /// Wiring the host functions (WASI) into the linker failed.
    #[error("failed to link host functions: {0}")]
    Link(String),

    /// A guest instance could not be created (bad imports, init trap, …).
    #[error("failed to instantiate guest {name:?}: {detail}")]
    Instantiate {
        /// The guest's registered name.
        name: String,
        /// The underlying instantiation error.
        detail: String,
    },

    /// A required export (`memory`, `kbk_alloc`, `kbk_run`) was absent or had the
    /// wrong type.
    #[error("guest {name:?} is missing the required export {export:?}: {detail}")]
    MissingExport {
        /// The guest's registered name.
        name: String,
        /// The export that was expected.
        export: String,
        /// The underlying lookup error.
        detail: String,
    },

    /// The guest trapped during execution (panic, fuel exhaustion, epoch
    /// timeout, or out-of-bounds access).
    #[error("guest {name:?} trapped: {detail}")]
    Trap {
        /// The guest's registered name.
        name: String,
        /// The trap detail.
        detail: String,
    },

    /// The guest returned a pointer/length the host could not read back.
    #[error("guest {name:?} returned an invalid result region: {detail}")]
    BadOutput {
        /// The guest's registered name.
        name: String,
        /// What went wrong reading the result.
        detail: String,
    },

    /// Dispatch was requested for a guest that is not in the registry.
    #[error("no guest registered under the name {0:?}")]
    GuestNotFound(String),
}

impl From<MeshError> for kanbrick_core::Error {
    fn from(e: MeshError) -> Self {
        match e {
            MeshError::GuestNotFound(name) => {
                kanbrick_core::Error::NotFound(format!("guest {name:?}"))
            }
            other => kanbrick_core::Error::Internal(other.to_string()),
        }
    }
}
