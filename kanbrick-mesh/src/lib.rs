//! # kanbrick-mesh
//!
//! WASM orchestration runtime and guest dispatch — Layer 2 (Nerves/Muscle).
//!
//! Phase 3 builds this directly on **wasmtime 45** with **WASIp1** guests and a
//! minimal, in-house Host-Guest ABI, rather than depending on the vendored
//! Tachyon-Mesh `core-host` (~50K LOC, Component Model / `wasip2`, eBPF/AI). See
//! [`docs/adr/0002-phase-3-wasm-runtime.md`] for the decision and evidence; this
//! mirrors how Phase 2 built `kanbrick-auth` on Ironclaw's primitives.
//!
//! ## Surface
//!
//! * [`MeshRuntime`] — the engine + guest registry; load `.wasm`, dispatch calls,
//!   and [`invoke`](MeshRuntime::invoke) guests with a host-authoritative context.
//! * [`MeshHost`] — the host's [`HostFunctions`](kanbrick_core::abi::HostFunctions)
//!   implementation servicing guest capability calls.
//! * [`Scheduler`] — immediate task dispatch with a wall-clock timeout and a
//!   per-guest concurrency limit (#25).
//! * [`EventBus`] — publish/subscribe with typed subscriptions and a replayable
//!   log (#27).
//! * [`RuntimeLimits`] — per-guest sandbox limits (memory, fuel, timeout).
//! * [`GuestInfo`] — a registered guest's name + version.
//! * [`GuestMetric`] — a snapshot of one guest's invocation counters (#63).
//! * [`AssetStore`] — content-addressed, air-gapped guest artifact store (#64).
//! * [`HostServices`] — the backend the `kbk_query_graph`/`kbk_emit_event`
//!   imports route through; [`LocalHostServices`] is the in-process graph+bus
//!   binding (#68, the seam for the control-plane/executor split).
//! * [`MeshError`] — the runtime error surface.
//!
//! The host↔guest calling convention (ADR-0002): guests export `memory`,
//! `kbk_alloc(len) -> ptr`, and `kbk_run(ptr, len) -> (out_ptr << 32 | out_len)`.
//! The typed `HostFunctions`/`GuestModule` ABI (#22) and the guest SDK (#39)
//! build typed JSON payloads on top of this substrate.
//!
//! [`docs/adr/0002-phase-3-wasm-runtime.md`]: https://github.com/Tnsr-Q/Kanbrick-V1/blob/main/docs/adr/0002-phase-3-wasm-runtime.md

pub mod assets;
pub mod error;
pub mod event;
pub mod host;
pub mod runtime;
pub mod scheduler;
pub mod services;

pub use assets::{AssetError, AssetRef, AssetStore};
pub use error::{MeshError, Result};
pub use event::{EventBus, SubscriptionId};
pub use host::MeshHost;
pub use runtime::{GuestInfo, GuestMetric, MeshRuntime, RuntimeLimits};
pub use scheduler::{RetryPolicy, Scheduler, SchedulerConfig, TaskId, TaskStatus, TriggerHandle};
pub use services::{HostServices, HostServicesError, LocalHostServices};
