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

    /// The guest trapped during execution (panic, out-of-bounds access, …). Use
    /// [`MeshError::Timeout`] / [`MeshError::ResourceLimited`] for the specific
    /// epoch and fuel kills.
    #[error("guest {name:?} trapped: {detail}")]
    Trap {
        /// The guest's registered name.
        name: String,
        /// The trap detail.
        detail: String,
    },

    /// The guest exceeded its wall-clock budget and was killed (epoch
    /// interruption, #25).
    #[error("guest {name:?} timed out and was killed")]
    Timeout {
        /// The guest's registered name.
        name: String,
    },

    /// The guest hit a resource ceiling and was killed — fuel exhaustion
    /// (runaway compute) or a memory-growth denial (#28).
    #[error("guest {name:?} exceeded its resource limits: {detail}")]
    ResourceLimited {
        /// The guest's registered name.
        name: String,
        /// Which limit was hit.
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

impl MeshError {
    /// Classify the error from a guest function call into the most specific
    /// [`MeshError`]: an epoch interruption becomes [`Timeout`](Self::Timeout),
    /// fuel exhaustion becomes [`ResourceLimited`](Self::ResourceLimited), and
    /// anything else a generic [`Trap`](Self::Trap).
    pub(crate) fn from_call(name: &str, stage: &str, err: &wasmtime::Error) -> Self {
        match err.downcast_ref::<wasmtime::Trap>() {
            Some(wasmtime::Trap::Interrupt) => MeshError::Timeout {
                name: name.to_string(),
            },
            Some(wasmtime::Trap::OutOfFuel) => MeshError::ResourceLimited {
                name: name.to_string(),
                detail: "fuel exhausted (runaway compute)".to_string(),
            },
            _ => MeshError::Trap {
                name: name.to_string(),
                detail: format!("{stage}: {err}"),
            },
        }
    }
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
