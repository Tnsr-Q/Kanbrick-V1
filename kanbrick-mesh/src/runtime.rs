//! The WASM orchestration runtime (issue #21, ADR-0002).
//!
//! [`MeshRuntime`] owns a wasmtime [`Engine`] configured for sandboxed guest
//! execution and a registry of compiled guest [`Module`]s. [`MeshRuntime::dispatch`]
//! runs a guest against an input buffer through the ADR-0002 calling convention
//! (`kbk_alloc` / `kbk_run` over guest linear memory) and returns the output
//! bytes.
//!
//! Each dispatch uses a fresh [`Store`] whose WASI context is locked down (no
//! filesystem, no network, no inherited stdio) and whose linear memory is capped
//! by [`RuntimeLimits`]. Fuel and epoch interruption are enabled on the engine so
//! the scheduler (#25) and resource-enforcement (#28) slices can bound execution;
//! in this slice fuel is provisioned generously and no epoch ticker runs.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use kanbrick_core::abi::{Event, GraphQuery, GuestRequest, GuestResponse};
use kanbrick_core::FirmContext;
use kanbrick_store::Store as GraphStore;
use wasmtime::{
    Caller, Config, Engine, Extern, Instance, Linker, Module, Store, StoreLimits,
    StoreLimitsBuilder,
};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

use crate::error::{MeshError, Result};
use crate::event::EventBus;
use crate::services::{HostServices, LocalHostServices};

/// The import module name the host functions are published under; guests declare
/// `#[link(wasm_import_module = "kanbrick")]` to use them.
const HOST_MODULE: &str = "kanbrick";

/// Per-guest sandbox limits. Defaults are the operator-approved values recorded
/// in ADR-0002.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeLimits {
    /// Maximum linear memory a guest may grow to, in bytes (enforced now, #28).
    pub max_memory_bytes: usize,
    /// Fuel units provisioned per dispatch (enforced now; exhaustion kill is #28).
    pub fuel: u64,
    /// Wall-clock budget per dispatch. Stored here and enforced by the epoch
    /// ticker introduced in the scheduler slice (#25); not enforced in #21.
    pub timeout: Duration,
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        RuntimeLimits {
            max_memory_bytes: 64 * 1024 * 1024,
            fuel: 1_000_000_000,
            timeout: Duration::from_secs(5),
        }
    }
}

/// Store-local host state for a single dispatch: the guest's WASI context, its
/// memory limiter, and the host-authoritative [`FirmContext`] in two forms — JSON
/// (`ctx_json`, read back through the `kbk_ctx_*` imports, #23) and typed (`ctx`,
/// handed to [`HostServices`] for the `kbk_query_graph` / `kbk_emit_event`
/// imports, #24/#27). `cap` is the optional per-invocation capability threaded to
/// a *remote* [`HostServices`] in the executor split (#70); it is `None`
/// in-process. `services` is the backend those two imports call.
///
/// `ctx`, `cap`, and `services` are all `None` for the raw
/// [`MeshRuntime::dispatch`] path, which wires no host imports.
struct HostState {
    wasi: WasiP1Ctx,
    limits: StoreLimits,
    ctx_json: Vec<u8>,
    ctx: Option<FirmContext>,
    cap: Option<String>,
    services: Option<Arc<dyn HostServices>>,
}

/// A compiled guest in the registry.
struct RegisteredGuest {
    name: String,
    version: String,
    module: Module,
}

/// Public, cloneable view of a registered guest (name + version).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestInfo {
    /// The guest's registered name.
    pub name: String,
    /// The guest's self-reported version.
    pub version: String,
}

/// Per-guest invocation counters (#63, Track A). All fields are atomic so they
/// can be read and updated under a shared (read) lock; `active` is signed so it
/// can be incremented when a call starts and decremented when it finishes,
/// including on the error and timeout paths.
#[derive(Debug, Default)]
struct GuestCounters {
    /// Calls currently executing (a gauge: up on entry, down on exit).
    active: AtomicI64,
    /// Calls that returned a response.
    completed: AtomicU64,
    /// Calls that failed (trap, bad output, missing guest, resource limit, …).
    failed: AtomicU64,
    /// Calls killed for exceeding their wall-clock budget.
    timed_out: AtomicU64,
}

/// Invocation metrics keyed by guest name (#63, Track A).
///
/// Counters are keyed by name rather than tied to a compiled [`Module`], so a
/// guest's totals are **continuous across a hot-reload** ([`MeshRuntime::reload_module`]).
/// Every invocation funnels through [`MeshRuntime::invoke_with_deadline`], so both
/// the direct API path and the [`Scheduler`](crate::Scheduler) (trigger/event)
/// path are accounted here.
#[derive(Debug, Default)]
struct MeshMetrics {
    guests: RwLock<HashMap<String, Arc<GuestCounters>>>,
}

/// A point-in-time snapshot of one guest's invocation counters (#63, Track A).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestMetric {
    /// The guest's registered name.
    pub name: String,
    /// Calls currently executing.
    pub active: i64,
    /// Calls that returned a response.
    pub completed: u64,
    /// Calls that failed (trap, bad output, missing guest, resource limit, …).
    pub failed: u64,
    /// Calls killed for exceeding their wall-clock budget.
    pub timed_out: u64,
}

/// The WASM runtime: a wasmtime engine plus a registry of loadable guests.
///
/// The registry is behind an [`RwLock`] so a guest can be hot-reloaded (#29)
/// while the runtime is concurrently serving calls: dispatch takes a read lock
/// and clones the (cheap, `Arc`-backed) [`Module`] before releasing it, so an
/// in-flight call always finishes on the code it started with even as a reload
/// swaps in a replacement for subsequent calls.
pub struct MeshRuntime {
    engine: Engine,
    limits: RuntimeLimits,
    registry: RwLock<HashMap<String, RegisteredGuest>>,
    /// The in-process host-services backing composed from
    /// [`with_store`](Self::with_store) / [`with_bus`](Self::with_bus).
    local: LocalHostServices,
    /// An explicit [`HostServices`] backend (e.g. the executor split's remote
    /// backend, #70). Takes precedence over `local` when set.
    services: Option<Arc<dyn HostServices>>,
    metrics: MeshMetrics,
}

impl MeshRuntime {
    /// Create a runtime with the default [`RuntimeLimits`].
    pub fn new() -> Result<Self> {
        Self::with_limits(RuntimeLimits::default())
    }

    /// Create a runtime with explicit sandbox limits.
    pub fn with_limits(limits: RuntimeLimits) -> Result<Self> {
        let mut config = Config::new();
        // Enable the metering primitives the scheduler/resource slices rely on.
        config.consume_fuel(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(|e| MeshError::Engine(e.to_string()))?;
        Ok(MeshRuntime {
            engine,
            limits,
            registry: RwLock::new(HashMap::new()),
            local: LocalHostServices::default(),
            services: None,
            metrics: MeshMetrics::default(),
        })
    }

    /// Bind the firm graph so guests' `query_graph` calls can run (#24), via the
    /// in-process [`LocalHostServices`]. Without a bound store (and no
    /// [`with_services`](Self::with_services) override), a `kbk_query_graph` call
    /// traps. Composes with [`with_bus`](Self::with_bus) in either order. Returns
    /// `self` for chaining.
    pub fn with_store(mut self, store: Arc<GraphStore>) -> Self {
        self.local = self.local.with_store(store);
        self
    }

    /// Bind an [`EventBus`] so guests' `emit` calls publish onto it (#27/#46),
    /// via the in-process [`LocalHostServices`]. Without a bound bus, an emitted
    /// event is logged and dropped. Builder-style.
    pub fn with_bus(mut self, bus: EventBus) -> Self {
        self.local = self.local.with_bus(bus);
        self
    }

    /// Bind an explicit [`HostServices`] backend for the `kbk_query_graph` and
    /// `kbk_emit_event` imports, overriding the in-process
    /// [`with_store`](Self::with_store) / [`with_bus`](Self::with_bus) backing.
    /// The executor split (#70) uses this to route those calls to a remote
    /// control plane. Builder-style.
    pub fn with_services(mut self, services: Arc<dyn HostServices>) -> Self {
        self.services = Some(services);
        self
    }

    /// The effective host services for a context-bearing dispatch: an explicit
    /// [`with_services`](Self::with_services) backend takes precedence; otherwise
    /// the in-process [`LocalHostServices`] if any backing is bound. `None` means
    /// a guest's `kbk_query_graph` traps and an emitted event is logged/dropped.
    fn host_services(&self) -> Option<Arc<dyn HostServices>> {
        if let Some(services) = &self.services {
            return Some(services.clone());
        }
        if self.local.is_bound() {
            return Some(Arc::new(self.local.clone()));
        }
        None
    }

    /// The sandbox limits applied to every dispatch.
    pub fn limits(&self) -> &RuntimeLimits {
        &self.limits
    }

    /// Compile `wasm` and register it under `name` with `version`. Used to load
    /// guests at setup; see [`reload_module`](Self::reload_module) for swapping a
    /// guest while the runtime is serving.
    pub fn register_module(&mut self, name: &str, version: &str, wasm: &[u8]) -> Result<()> {
        self.install(name, version, wasm)
    }

    /// Hot-reload the guest registered as `name`, **replacing** it with `wasm`
    /// compiled at `version` (#29). Callable while the runtime is concurrently
    /// serving (it takes `&self`).
    ///
    /// The swap is atomic and fail-safe: the new module is compiled *first*, and
    /// the registry is only updated if compilation succeeds. A corrupt or invalid
    /// replacement is rejected with an error and the previously-registered guest
    /// keeps serving. In-flight calls on the old module are unaffected (they hold
    /// their own `Arc`-clone); subsequent calls route to the replacement, with
    /// nothing dropped.
    pub fn reload_module(&self, name: &str, version: &str, wasm: &[u8]) -> Result<()> {
        self.install(name, version, wasm)
    }

    /// Compile-then-swap a guest into the registry. Compilation happens *before*
    /// the write lock, so a bad module never disturbs the registry or blocks
    /// concurrent dispatch.
    fn install(&self, name: &str, version: &str, wasm: &[u8]) -> Result<()> {
        let module = Module::new(&self.engine, wasm).map_err(|e| MeshError::Compile {
            name: name.to_string(),
            detail: e.to_string(),
        })?;
        self.registry.write().expect("registry lock").insert(
            name.to_string(),
            RegisteredGuest {
                name: name.to_string(),
                version: version.to_string(),
                module,
            },
        );
        // Ensure a metrics entry exists for the guest. `or_default` preserves any
        // existing counters, so totals carry across a hot-reload (#29).
        self.metrics
            .guests
            .write()
            .expect("metrics lock")
            .entry(name.to_string())
            .or_default();
        tracing::debug!(target: "kanbrick_mesh::registry", guest = name, version, "registered guest");
        Ok(())
    }

    /// The guests currently in the registry, sorted by name for determinism.
    pub fn guests(&self) -> Vec<GuestInfo> {
        let mut out: Vec<GuestInfo> = self
            .registry
            .read()
            .expect("registry lock")
            .values()
            .map(|g| GuestInfo {
                name: g.name.clone(),
                version: g.version.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Whether a guest is registered under `name`.
    pub fn contains(&self, name: &str) -> bool {
        self.registry
            .read()
            .expect("registry lock")
            .contains_key(name)
    }

    /// Run guest `name` against raw `input` bytes, returning the bytes it
    /// produces. This is the low-level #21 substrate with **no** host context
    /// or imports; see [`invoke`](Self::invoke) for the typed, context-bearing
    /// entry point.
    ///
    /// A fresh, sandboxed instance is created for the call and dropped afterward.
    pub fn dispatch(&self, name: &str, input: &[u8]) -> Result<Vec<u8>> {
        let module = self.module(name)?;
        let mut store = self.new_store(Vec::new(), None, None, None, u64::MAX)?;
        let mut linker: Linker<HostState> = Linker::new(&self.engine);
        self.add_wasi(&mut linker)?;
        let instance = self.instantiate_guest(&mut store, name, &module, &linker)?;
        self.call_run(&mut store, &instance, name, input)
    }

    /// Invoke guest `name` on behalf of `ctx` with a typed [`GuestRequest`],
    /// returning its [`GuestResponse`].
    ///
    /// The caller's [`FirmContext`] is **host-authoritative** (#23): it is held
    /// by the host and exposed to the guest only through the read-only
    /// `kbk_ctx_*` imports. Nothing in `request` can set or forge identity.
    pub fn invoke(
        &self,
        name: &str,
        ctx: &FirmContext,
        request: &GuestRequest,
    ) -> Result<GuestResponse> {
        self.invoke_with_cap(name, ctx, None, request)
    }

    /// Like [`invoke`](Self::invoke) but threading a per-invocation capability
    /// `cap` to the bound [`HostServices`]. In the executor
    /// split (#70) the cap is the bearer token a *remote* backend relays to the
    /// control plane so a guest's `kbk_query_graph` / `kbk_emit_event` callbacks
    /// run under the caller's host-authoritative identity across the network hop.
    /// In-process the cap is ignored (identity is already local).
    pub fn invoke_with_cap(
        &self,
        name: &str,
        ctx: &FirmContext,
        cap: Option<&str>,
        request: &GuestRequest,
    ) -> Result<GuestResponse> {
        self.invoke_with_deadline_cap(name, ctx, cap, request, u64::MAX)
    }

    /// Like [`invoke`](Self::invoke) but the guest is killed after `epoch_deadline`
    /// engine ticks (`u64::MAX` = unbounded). Crate-internal: the
    /// [`Scheduler`](crate::Scheduler) converts a task timeout into ticks and
    /// drives the engine epoch so the deadline actually fires (#25).
    pub(crate) fn invoke_with_deadline(
        &self,
        name: &str,
        ctx: &FirmContext,
        request: &GuestRequest,
        epoch_deadline: u64,
    ) -> Result<GuestResponse> {
        self.invoke_with_deadline_cap(name, ctx, None, request, epoch_deadline)
    }

    /// The metric-accounted invocation core, threading the optional per-invocation
    /// `cap`. Both the capability-bearing [`invoke_with_cap`](Self::invoke_with_cap)
    /// and the deadline-bearing [`invoke_with_deadline`](Self::invoke_with_deadline)
    /// funnel through here, so every typed call (direct API path, executor path,
    /// and Scheduler) is counted in the pressure metrics (#63).
    fn invoke_with_deadline_cap(
        &self,
        name: &str,
        ctx: &FirmContext,
        cap: Option<&str>,
        request: &GuestRequest,
        epoch_deadline: u64,
    ) -> Result<GuestResponse> {
        // Account the invocation here, the single choke point every typed call
        // funnels through (direct API path *and* the Scheduler), so the pressure
        // metrics (#63) cover both. Unknown guests have no counter — they fail
        // below with `GuestNotFound` and are not tracked (no unbounded growth).
        let counters = self.counters_for(name);
        if let Some(c) = &counters {
            c.active.fetch_add(1, Ordering::Relaxed);
        }
        let result = self.invoke_inner(name, ctx, cap, request, epoch_deadline);
        if let Some(c) = &counters {
            c.active.fetch_sub(1, Ordering::Relaxed);
            match &result {
                Ok(_) => {
                    c.completed.fetch_add(1, Ordering::Relaxed);
                }
                Err(MeshError::Timeout { .. }) => {
                    c.timed_out.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    c.failed.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        result
    }

    /// The actual typed invocation: encode the request, run the guest, decode the
    /// response. Wrapped by [`invoke_with_deadline_cap`](Self::invoke_with_deadline_cap),
    /// which records the metrics around it.
    fn invoke_inner(
        &self,
        name: &str,
        ctx: &FirmContext,
        cap: Option<&str>,
        request: &GuestRequest,
        epoch_deadline: u64,
    ) -> Result<GuestResponse> {
        let input = request.to_json_bytes().map_err(|e| MeshError::BadOutput {
            name: name.to_string(),
            detail: format!("encoding request: {e}"),
        })?;
        let output = self.run_with_context_deadline(name, ctx, cap, &input, epoch_deadline)?;
        GuestResponse::from_json_bytes(&output).map_err(|e| MeshError::BadOutput {
            name: name.to_string(),
            detail: format!("decoding response: {e}"),
        })
    }

    /// Fetch a registered guest's counters, or `None` if the guest is unknown.
    /// Registered guests always have an entry (created in [`install`](Self::install)),
    /// so a hit here means the call will actually be dispatched.
    fn counters_for(&self, name: &str) -> Option<Arc<GuestCounters>> {
        self.metrics
            .guests
            .read()
            .expect("metrics lock")
            .get(name)
            .map(Arc::clone)
    }

    /// A snapshot of every registered guest's invocation counters, sorted by name
    /// for deterministic output (#63, Track A). Consumed by `kanbrick-api`'s
    /// `/metrics` endpoint.
    pub fn metrics_snapshot(&self) -> Vec<GuestMetric> {
        let mut out: Vec<GuestMetric> = self
            .metrics
            .guests
            .read()
            .expect("metrics lock")
            .iter()
            .map(|(name, c)| GuestMetric {
                name: name.clone(),
                active: c.active.load(Ordering::Relaxed),
                completed: c.completed.load(Ordering::Relaxed),
                failed: c.failed.load(Ordering::Relaxed),
                timed_out: c.timed_out.load(Ordering::Relaxed),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Run guest `name` against raw `input`, injecting `ctx` as the
    /// host-authoritative [`FirmContext`] readable through the `kbk_ctx_*` host
    /// imports. Returns the raw output bytes. ([`invoke`](Self::invoke) is the
    /// typed wrapper.)
    pub fn run_with_context(&self, name: &str, ctx: &FirmContext, input: &[u8]) -> Result<Vec<u8>> {
        self.run_with_context_deadline(name, ctx, None, input, u64::MAX)
    }

    /// [`run_with_context`](Self::run_with_context) with an explicit epoch
    /// deadline (`u64::MAX` = unbounded) and an optional per-invocation `cap`
    /// threaded to the bound [`HostServices`](crate::HostServices) (#70).
    fn run_with_context_deadline(
        &self,
        name: &str,
        ctx: &FirmContext,
        cap: Option<&str>,
        input: &[u8],
        epoch_deadline: u64,
    ) -> Result<Vec<u8>> {
        let ctx_json = serde_json::to_vec(ctx)
            .map_err(|e| MeshError::Engine(format!("serializing firm context: {e}")))?;
        let module = self.module(name)?;
        let services = self.host_services();
        let mut store = self.new_store(
            ctx_json,
            Some(ctx.clone()),
            cap.map(|c| c.to_string()),
            services,
            epoch_deadline,
        )?;
        let mut linker: Linker<HostState> = Linker::new(&self.engine);
        self.add_wasi(&mut linker)?;
        self.add_context_imports(&mut linker)?;
        self.add_query_imports(&mut linker)?;
        self.add_event_imports(&mut linker)?;
        self.add_log_imports(&mut linker)?;
        let instance = self.instantiate_guest(&mut store, name, &module, &linker)?;
        self.call_run(&mut store, &instance, name, input)
    }

    /// Clone a registered guest's compiled module (cheap — `Module` is
    /// `Arc`-backed) under a brief read lock, so an in-flight call is decoupled
    /// from any concurrent hot-reload (#29).
    fn module(&self, name: &str) -> Result<Module> {
        self.registry
            .read()
            .expect("registry lock")
            .get(name)
            .map(|g| g.module.clone())
            .ok_or_else(|| MeshError::GuestNotFound(name.to_string()))
    }

    /// Add the locked-down WASIp1 host functions to `linker`.
    fn add_wasi(&self, linker: &mut Linker<HostState>) -> Result<()> {
        wasmtime_wasi::p1::add_to_linker_sync(linker, |s: &mut HostState| &mut s.wasi)
            .map_err(|e| MeshError::Link(e.to_string()))
    }

    /// Publish the host-authoritative context imports (#23): `kbk_ctx_len` and
    /// `kbk_ctx_read`, the *only* way a guest learns its caller's identity. There
    /// is deliberately no import to *set* the context.
    fn add_context_imports(&self, linker: &mut Linker<HostState>) -> Result<()> {
        linker
            .func_wrap(
                HOST_MODULE,
                "kbk_ctx_len",
                |caller: Caller<'_, HostState>| -> u32 { caller.data().ctx_json.len() as u32 },
            )
            .map_err(|e| MeshError::Link(e.to_string()))?;
        linker
            .func_wrap(
                HOST_MODULE,
                "kbk_ctx_read",
                |mut caller: Caller<'_, HostState>, ptr: u32| -> wasmtime::Result<()> {
                    let json = caller.data().ctx_json.clone();
                    let memory = guest_memory(&mut caller)?;
                    memory.write(&mut caller, ptr as usize, &json)?;
                    Ok(())
                },
            )
            .map_err(|e| MeshError::Link(e.to_string()))?;
        Ok(())
    }

    /// Publish the graph-query import (#24): `kbk_query_graph(in_ptr, in_len)`,
    /// which the host services through the bound [`HostServices`] — the in-process
    /// [`LocalHostServices`](crate::LocalHostServices) runs the query through a
    /// clearance-enforcing `GuardedStore` — returning the packed
    /// `(out_ptr, out_len)` of the JSON `GraphRows` written back into guest memory.
    ///
    /// This import is re-entrant: to return a variable-length result the host
    /// calls back into the guest's own `kbk_alloc`. If no services are bound, the
    /// call traps (a guest cannot query a runtime with no graph).
    fn add_query_imports(&self, linker: &mut Linker<HostState>) -> Result<()> {
        linker
            .func_wrap(
                HOST_MODULE,
                "kbk_query_graph",
                |mut caller: Caller<'_, HostState>,
                 in_ptr: u32,
                 in_len: u32|
                 -> wasmtime::Result<u64> {
                    let memory = guest_memory(&mut caller)?;

                    // Read the request JSON the guest wrote into its memory.
                    let mut buf = vec![0u8; in_len as usize];
                    memory.read(&caller, in_ptr as usize, &mut buf)?;
                    let query: GraphQuery = serde_json::from_slice(&buf)
                        .map_err(|e| wasmtime::Error::msg(format!("invalid GraphQuery: {e}")))?;

                    // Resolve the host-authoritative caller context, the
                    // per-invocation capability, and the backing services. Clone
                    // them out up front so nothing borrows `caller` across the
                    // service call (which may re-enter the guest below).
                    let services =
                        caller.data().services.clone().ok_or_else(|| {
                            wasmtime::Error::msg("no graph bound to this runtime")
                        })?;
                    let ctx = caller
                        .data()
                        .ctx
                        .clone()
                        .ok_or_else(|| wasmtime::Error::msg("no caller context bound"))?;
                    let cap = caller.data().cap.clone();

                    // Run it through the (clearance-enforcing) services, then
                    // JSON-encode the rows for the guest.
                    let rows = services
                        .query_graph(&ctx, cap.as_deref(), &query)
                        .map_err(|e| wasmtime::Error::msg(e.to_string()))?;
                    let out = serde_json::to_vec(&rows)
                        .map_err(|e| wasmtime::Error::msg(e.to_string()))?;

                    // Hand the result back: allocate space in *guest* memory via
                    // the guest's own allocator, then write the rows there.
                    let alloc = caller
                        .get_export("kbk_alloc")
                        .and_then(Extern::into_func)
                        .ok_or_else(|| wasmtime::Error::msg("guest has no kbk_alloc export"))?
                        .typed::<u32, u32>(&caller)?;
                    let out_len = u32::try_from(out.len())
                        .map_err(|_| wasmtime::Error::msg("query result exceeds 4 GiB"))?;
                    let out_ptr = alloc.call(&mut caller, out_len)?;
                    let memory = guest_memory(&mut caller)?;
                    memory.write(&mut caller, out_ptr as usize, &out)?;

                    Ok(((out_ptr as u64) << 32) | (out_len as u64))
                },
            )
            .map_err(|e| MeshError::Link(e.to_string()))?;
        Ok(())
    }

    /// Publish the event import (#27/#46): `kbk_emit_event(in_ptr, in_len)` reads
    /// the [`Event`] JSON the guest wrote and publishes it through the bound
    /// [`HostServices`] — the in-process [`LocalHostServices`](crate::LocalHostServices)
    /// publishes it onto the bound [`EventBus`]. With no services (and so no bus)
    /// bound the event is logged and dropped (never retained), matching the
    /// host-side [`MeshHost`](crate::MeshHost) behaviour.
    fn add_event_imports(&self, linker: &mut Linker<HostState>) -> Result<()> {
        linker
            .func_wrap(
                HOST_MODULE,
                "kbk_emit_event",
                |mut caller: Caller<'_, HostState>,
                 in_ptr: u32,
                 in_len: u32|
                 -> wasmtime::Result<()> {
                    let memory = guest_memory(&mut caller)?;
                    let mut buf = vec![0u8; in_len as usize];
                    memory.read(&caller, in_ptr as usize, &mut buf)?;
                    let event: Event = serde_json::from_slice(&buf)
                        .map_err(|e| wasmtime::Error::msg(format!("invalid Event: {e}")))?;
                    // Route through the backing services on behalf of the
                    // host-authoritative caller. With no services (and so no bus)
                    // bound, the event is logged and dropped — never retained.
                    let services = caller.data().services.clone();
                    let ctx = caller.data().ctx.clone();
                    let cap = caller.data().cap.clone();
                    match (services, ctx) {
                        (Some(services), Some(ctx)) => {
                            services
                                .emit_event(&ctx, cap.as_deref(), &event)
                                .map_err(|e| wasmtime::Error::msg(e.to_string()))?;
                        }
                        _ => tracing::info!(
                            target: "kanbrick_mesh::guest",
                            kind = %event.kind,
                            "guest emitted an event but no bus is bound (dropped)"
                        ),
                    }
                    Ok(())
                },
            )
            .map_err(|e| MeshError::Link(e.to_string()))?;
        Ok(())
    }

    /// Publish the log import: `kbk_log(level, in_ptr, in_len)` records the UTF-8
    /// message at the guest's chosen [`LogLevel`](kanbrick_core::abi::LogLevel)
    /// (encoded `0=Error..4=Trace`) onto the host tracing target.
    fn add_log_imports(&self, linker: &mut Linker<HostState>) -> Result<()> {
        linker
            .func_wrap(
                HOST_MODULE,
                "kbk_log",
                |mut caller: Caller<'_, HostState>,
                 level: u32,
                 in_ptr: u32,
                 in_len: u32|
                 -> wasmtime::Result<()> {
                    let memory = guest_memory(&mut caller)?;
                    let mut buf = vec![0u8; in_len as usize];
                    memory.read(&caller, in_ptr as usize, &mut buf)?;
                    let message = String::from_utf8_lossy(&buf);
                    match level {
                        0 => tracing::error!(target: "kanbrick_mesh::guest", "{message}"),
                        1 => tracing::warn!(target: "kanbrick_mesh::guest", "{message}"),
                        2 => tracing::info!(target: "kanbrick_mesh::guest", "{message}"),
                        3 => tracing::debug!(target: "kanbrick_mesh::guest", "{message}"),
                        _ => tracing::trace!(target: "kanbrick_mesh::guest", "{message}"),
                    }
                    Ok(())
                },
            )
            .map_err(|e| MeshError::Link(e.to_string()))?;
        Ok(())
    }

    /// Instantiate `module` and run its `_initialize` (reactor) export if present.
    fn instantiate_guest(
        &self,
        store: &mut Store<HostState>,
        name: &str,
        module: &Module,
        linker: &Linker<HostState>,
    ) -> Result<Instance> {
        let instance =
            linker
                .instantiate(&mut *store, module)
                .map_err(|e| MeshError::Instantiate {
                    name: name.to_string(),
                    detail: e.to_string(),
                })?;
        // Reactor guests (cdylib) export `_initialize`; run it once if present.
        if let Ok(init) = instance.get_typed_func::<(), ()>(&mut *store, "_initialize") {
            init.call(&mut *store, ()).map_err(|e| MeshError::Trap {
                name: name.to_string(),
                detail: format!("_initialize: {e}"),
            })?;
        }
        Ok(instance)
    }

    /// Drive the ADR-0002 calling convention: `kbk_alloc` input, write it,
    /// `kbk_run`, then read back the packed `(ptr, len)` output region.
    fn call_run(
        &self,
        store: &mut Store<HostState>,
        instance: &Instance,
        name: &str,
        input: &[u8],
    ) -> Result<Vec<u8>> {
        let memory =
            instance
                .get_memory(&mut *store, "memory")
                .ok_or_else(|| MeshError::MissingExport {
                    name: name.to_string(),
                    export: "memory".to_string(),
                    detail: "not exported".to_string(),
                })?;
        let alloc = instance
            .get_typed_func::<u32, u32>(&mut *store, "kbk_alloc")
            .map_err(|e| MeshError::MissingExport {
                name: name.to_string(),
                export: "kbk_alloc".to_string(),
                detail: e.to_string(),
            })?;
        let run = instance
            .get_typed_func::<(u32, u32), u64>(&mut *store, "kbk_run")
            .map_err(|e| MeshError::MissingExport {
                name: name.to_string(),
                export: "kbk_run".to_string(),
                detail: e.to_string(),
            })?;

        let in_len = u32::try_from(input.len()).map_err(|_| MeshError::BadOutput {
            name: name.to_string(),
            detail: "input exceeds 4 GiB".to_string(),
        })?;

        let in_ptr = alloc
            .call(&mut *store, in_len)
            .map_err(|e| MeshError::from_call(name, "kbk_alloc", &e))?;
        memory
            .write(&mut *store, in_ptr as usize, input)
            .map_err(|e| MeshError::BadOutput {
                name: name.to_string(),
                detail: format!("writing input: {e}"),
            })?;

        let packed = run
            .call(&mut *store, (in_ptr, in_len))
            .map_err(|e| MeshError::from_call(name, "kbk_run", &e))?;

        let out_ptr = (packed >> 32) as usize;
        let out_len = (packed & 0xffff_ffff) as usize;
        let mut out = vec![0u8; out_len];
        memory
            .read(&*store, out_ptr, &mut out)
            .map_err(|e| MeshError::BadOutput {
                name: name.to_string(),
                detail: format!("reading {out_len} bytes at {out_ptr}: {e}"),
            })?;

        tracing::debug!(
            target: "kanbrick_mesh::dispatch",
            guest = name,
            in_len,
            out_len,
            "dispatched guest call"
        );
        Ok(out)
    }

    /// Build a fresh, sandboxed [`Store`] for a single dispatch, carrying the
    /// (possibly empty) host-authoritative context (JSON + typed), the optional
    /// per-invocation capability, and the optional [`HostServices`] backend, and
    /// armed with `epoch_deadline` engine ticks before it is killed (`u64::MAX` =
    /// no wall-clock limit). The [`Scheduler`](crate::Scheduler) drives the engine
    /// epoch that makes a finite deadline fire (#25).
    fn new_store(
        &self,
        ctx_json: Vec<u8>,
        ctx: Option<FirmContext>,
        cap: Option<String>,
        services: Option<Arc<dyn HostServices>>,
        epoch_deadline: u64,
    ) -> Result<Store<HostState>> {
        // Locked-down WASI: no preopened dirs, no network, no inherited stdio.
        let wasi = WasiCtxBuilder::new().build_p1();
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.limits.max_memory_bytes)
            .build();
        let mut store = Store::new(
            &self.engine,
            HostState {
                wasi,
                limits,
                ctx_json,
                ctx,
                cap,
                services,
            },
        );
        store.limiter(|s| &mut s.limits);
        store
            .set_fuel(self.limits.fuel)
            .map_err(|e| MeshError::Engine(e.to_string()))?;
        // `set_epoch_deadline` adds `ticks_beyond_current` to the engine's current
        // epoch; once the scheduler's ticker has advanced that epoch, an unbounded
        // `u64::MAX` deadline would overflow (and panic in debug). Clamp so the
        // "no timeout" sentinel stays effectively infinite without overflowing.
        store.set_epoch_deadline(epoch_deadline.min(u64::MAX / 2));
        Ok(store)
    }

    /// The wasmtime engine. Crate-internal so the [`Scheduler`](crate::Scheduler)
    /// can drive epoch interruption for timeouts.
    pub(crate) fn engine(&self) -> &Engine {
        &self.engine
    }
}

/// Fetch the guest's exported `memory` from within a host import.
fn guest_memory(caller: &mut Caller<'_, HostState>) -> wasmtime::Result<wasmtime::Memory> {
    match caller.get_export("memory") {
        Some(Extern::Memory(m)) => Ok(m),
        _ => Err(wasmtime::Error::msg("guest has no exported memory")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hermetic in-memory echo module (no WASI imports, no toolchain needed):
    /// a bump allocator plus a `kbk_run` that echoes its input in place.
    const ECHO_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (global $next (mut i32) (i32.const 1024))
          (func (export "kbk_alloc") (param $len i32) (result i32)
            (local $p i32)
            global.get $next
            local.set $p
            global.get $next
            local.get $len
            i32.add
            global.set $next
            local.get $p)
          (func (export "kbk_run") (param $ptr i32) (param $len i32) (result i64)
            local.get $ptr
            i64.extend_i32_u
            i64.const 32
            i64.shl
            local.get $len
            i64.extend_i32_u
            i64.or))
    "#;

    fn runtime_with_echo() -> MeshRuntime {
        let mut rt = MeshRuntime::new().unwrap();
        rt.register_module("echo", "0.0.0", ECHO_WAT.as_bytes())
            .unwrap();
        rt
    }

    #[test]
    fn new_runtime_has_empty_registry() {
        let rt = MeshRuntime::new().unwrap();
        assert!(rt.guests().is_empty());
        assert!(!rt.contains("echo"));
    }

    #[test]
    fn registry_lists_name_and_version() {
        let rt = runtime_with_echo();
        assert!(rt.contains("echo"));
        assert_eq!(
            rt.guests(),
            vec![GuestInfo {
                name: "echo".to_string(),
                version: "0.0.0".to_string()
            }]
        );
    }

    #[test]
    fn dispatch_echoes_bytes() {
        let rt = runtime_with_echo();
        assert_eq!(rt.dispatch("echo", b"hello").unwrap(), b"hello");
        assert_eq!(rt.dispatch("echo", b"").unwrap(), b"");
        let big = vec![7u8; 4096];
        assert_eq!(rt.dispatch("echo", &big).unwrap(), big);
    }

    #[test]
    fn dispatch_unknown_guest_errors() {
        let rt = runtime_with_echo();
        let err = rt.dispatch("missing", b"x").unwrap_err();
        assert!(matches!(err, MeshError::GuestNotFound(n) if n == "missing"));
    }

    #[test]
    fn invalid_wasm_fails_to_register() {
        let mut rt = MeshRuntime::new().unwrap();
        let err = rt
            .register_module("bad", "0.0.0", b"\0not wasm")
            .unwrap_err();
        assert!(matches!(err, MeshError::Compile { .. }));
    }

    #[test]
    fn default_limits_match_adr() {
        let rt = MeshRuntime::new().unwrap();
        assert_eq!(rt.limits().max_memory_bytes, 64 * 1024 * 1024);
        assert_eq!(rt.limits().fuel, 1_000_000_000);
        assert_eq!(rt.limits().timeout, Duration::from_secs(5));
    }

    // ---- #23: host-authoritative FirmContext propagation. ----

    /// A hermetic guest that imports the host context functions, reads the
    /// injected [`FirmContext`] JSON, and returns it verbatim — proving the host
    /// supplies identity and the guest can only *read* it.
    const CTX_WAT: &str = r#"
        (module
          (import "kanbrick" "kbk_ctx_len" (func $ctx_len (result i32)))
          (import "kanbrick" "kbk_ctx_read" (func $ctx_read (param i32)))
          (memory (export "memory") 1)
          (global $next (mut i32) (i32.const 1024))
          (func (export "kbk_alloc") (param $len i32) (result i32)
            (local $p i32)
            global.get $next
            local.set $p
            global.get $next
            local.get $len
            i32.add
            global.set $next
            local.get $p)
          (func (export "kbk_run") (param $ptr i32) (param $len i32) (result i64)
            (local $clen i32)
            (local $cptr i32)
            call $ctx_len
            local.set $clen
            global.get $next
            local.set $cptr
            global.get $next
            local.get $clen
            i32.add
            global.set $next
            local.get $cptr
            call $ctx_read
            local.get $cptr
            i64.extend_i32_u
            i64.const 32
            i64.shl
            local.get $clen
            i64.extend_i32_u
            i64.or))
    "#;

    fn ctx(email: &str, clearance: kanbrick_core::ClearanceLevel) -> FirmContext {
        FirmContext::new(uuid::Uuid::new_v4(), email, clearance)
    }

    fn runtime_with_ctx_probe() -> MeshRuntime {
        let mut rt = MeshRuntime::new().unwrap();
        rt.register_module("ctx", "0.0.0", CTX_WAT.as_bytes())
            .unwrap();
        rt
    }

    #[test]
    fn run_with_context_exposes_host_authoritative_identity() {
        let rt = runtime_with_ctx_probe();
        let context = ctx("lead@kanbrick.com", kanbrick_core::ClearanceLevel::L4);
        // Input is ignored by the probe; it returns the host-injected context.
        let out = rt
            .run_with_context("ctx", &context, b"ignored input")
            .unwrap();
        let seen: FirmContext = serde_json::from_slice(&out).unwrap();
        assert_eq!(seen, context);
    }

    #[test]
    fn guest_sees_exactly_the_context_the_host_injects() {
        let rt = runtime_with_ctx_probe();
        let a = ctx("a@kanbrick.com", kanbrick_core::ClearanceLevel::L2);
        let b = ctx("b@kanbrick.com", kanbrick_core::ClearanceLevel::L5);
        let seen_a: FirmContext =
            serde_json::from_slice(&rt.run_with_context("ctx", &a, b"").unwrap()).unwrap();
        let seen_b: FirmContext =
            serde_json::from_slice(&rt.run_with_context("ctx", &b, b"").unwrap()).unwrap();
        // Each guest run sees precisely the identity the host chose — nothing the
        // guest does can change which context it is handed.
        assert_eq!(seen_a, a);
        assert_eq!(seen_b, b);
        assert_ne!(seen_a.clearance, seen_b.clearance);
    }

    #[test]
    fn invoke_round_trips_request_payload_through_a_guest() {
        // The hermetic echo guest returns its input verbatim; because GuestRequest
        // and GuestResponse share the `{ "payload": .. }` shape, echoing a request
        // yields the matching response — exercising invoke()'s encode/decode.
        let rt = runtime_with_echo();
        let context = ctx("analyst@kanbrick.com", kanbrick_core::ClearanceLevel::L3);
        let request = GuestRequest::new(serde_json::json!({"company_id": 7}));
        let response = rt.invoke("echo", &context, &request).unwrap();
        assert_eq!(response.payload, serde_json::json!({"company_id": 7}));
    }

    #[test]
    fn invoke_on_unknown_guest_errors() {
        let rt = MeshRuntime::new().unwrap();
        let context = ctx("x@kanbrick.com", kanbrick_core::ClearanceLevel::L1);
        let err = rt
            .invoke(
                "nope",
                &context,
                &GuestRequest::new(serde_json::Value::Null),
            )
            .unwrap_err();
        assert!(matches!(err, MeshError::GuestNotFound(n) if n == "nope"));
    }

    // ---- #63 (Track A): per-guest invocation metrics. ----

    /// A hermetic guest that always traps in `kbk_run` (proves failure counting).
    const TRAP_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "kbk_alloc") (param $len i32) (result i32) i32.const 1024)
          (func (export "kbk_run") (param $ptr i32) (param $len i32) (result i64)
            unreachable))
    "#;

    fn metric_for<'a>(snapshot: &'a [GuestMetric], name: &str) -> &'a GuestMetric {
        snapshot
            .iter()
            .find(|m| m.name == name)
            .unwrap_or_else(|| panic!("no metric for {name} in {snapshot:?}"))
    }

    #[test]
    fn registered_guest_starts_with_zeroed_metrics() {
        let rt = runtime_with_echo();
        let snap = rt.metrics_snapshot();
        let echo = metric_for(&snap, "echo");
        assert_eq!(echo.active, 0);
        assert_eq!(echo.completed, 0);
        assert_eq!(echo.failed, 0);
        assert_eq!(echo.timed_out, 0);
    }

    #[test]
    fn successful_invocations_count_as_completed() {
        let rt = runtime_with_echo();
        let context = ctx("analyst@kanbrick.com", kanbrick_core::ClearanceLevel::L3);
        for _ in 0..3 {
            rt.invoke(
                "echo",
                &context,
                &GuestRequest::new(serde_json::json!({"n": 1})),
            )
            .unwrap();
        }
        let snap = rt.metrics_snapshot();
        let echo = metric_for(&snap, "echo");
        assert_eq!(echo.completed, 3);
        assert_eq!(echo.active, 0, "gauge returns to zero after each call");
        assert_eq!(echo.failed, 0);
    }

    #[test]
    fn trapping_invocation_counts_as_failed() {
        let mut rt = MeshRuntime::new().unwrap();
        rt.register_module("trap", "0.0.0", TRAP_WAT.as_bytes())
            .unwrap();
        let context = ctx("x@kanbrick.com", kanbrick_core::ClearanceLevel::L1);
        let err = rt
            .invoke(
                "trap",
                &context,
                &GuestRequest::new(serde_json::Value::Null),
            )
            .unwrap_err();
        assert!(matches!(err, MeshError::Trap { .. }));
        let snap = rt.metrics_snapshot();
        let trap = metric_for(&snap, "trap");
        assert_eq!(trap.failed, 1);
        assert_eq!(trap.completed, 0);
        assert_eq!(trap.active, 0);
    }

    #[test]
    fn unknown_guest_is_not_tracked() {
        let rt = runtime_with_echo();
        let context = ctx("x@kanbrick.com", kanbrick_core::ClearanceLevel::L1);
        let _ = rt.invoke(
            "ghost",
            &context,
            &GuestRequest::new(serde_json::Value::Null),
        );
        // No counter is created for a guest that was never registered.
        assert!(rt.metrics_snapshot().iter().all(|m| m.name != "ghost"));
    }

    #[test]
    fn scheduler_path_invocations_are_counted() {
        // The Scheduler dispatches through `invoke_with_deadline` too, so its
        // (trigger/event) invocations land in the same per-guest counters — the
        // pressure signal is not blind to background work (#63).
        use crate::Scheduler;
        use std::time::Duration;

        let rt = Arc::new(runtime_with_echo());
        let scheduler = Scheduler::new(rt.clone());
        let context = ctx("analyst@kanbrick.com", kanbrick_core::ClearanceLevel::L3);
        let id = scheduler.schedule(
            "echo",
            &context,
            &GuestRequest::new(serde_json::json!({"via": "scheduler"})),
            None,
        );
        let status = scheduler
            .wait(id, Duration::from_secs(5))
            .expect("task reaches a terminal state");
        assert!(status.is_terminal());
        let snap = rt.metrics_snapshot();
        let echo = metric_for(&snap, "echo");
        assert_eq!(echo.completed, 1, "scheduler-driven call was counted");
    }

    // ---- #68 (Track D): the graph/event imports route through `HostServices`. ----

    /// A query-proxy guest: forwards its input bytes to `kbk_query_graph` and
    /// returns the resulting rows verbatim. Hermetic (no toolchain), mirroring the
    /// integration `guest_query` test's proxy.
    const QUERY_PROXY_WAT: &str = r#"
        (module
          (import "kanbrick" "kbk_query_graph" (func $query (param i32 i32) (result i64)))
          (memory (export "memory") 1)
          (global $next (mut i32) (i32.const 1024))
          (func (export "kbk_alloc") (param $len i32) (result i32)
            (local $p i32)
            global.get $next
            local.set $p
            global.get $next
            local.get $len
            i32.add
            global.set $next
            local.get $p)
          (func (export "kbk_run") (param $ptr i32) (param $len i32) (result i64)
            local.get $ptr
            local.get $len
            call $query))
    "#;

    /// An emit-proxy guest: forwards its input bytes (an `Event` JSON) to
    /// `kbk_emit_event` and returns an empty result region.
    const EMIT_PROXY_WAT: &str = r#"
        (module
          (import "kanbrick" "kbk_emit_event" (func $emit (param i32 i32)))
          (memory (export "memory") 1)
          (global $next (mut i32) (i32.const 1024))
          (func (export "kbk_alloc") (param $len i32) (result i32)
            (local $p i32)
            global.get $next
            local.set $p
            global.get $next
            local.get $len
            i32.add
            global.set $next
            local.get $p)
          (func (export "kbk_run") (param $ptr i32) (param $len i32) (result i64)
            local.get $ptr
            local.get $len
            call $emit
            i64.const 0))
    "#;

    /// A mock [`HostServices`] that records the context it is queried with and the
    /// events it is handed, and returns a canned set of rows for queries. Proves
    /// the imports dispatch through the trait rather than a concrete store/bus.
    #[derive(Default)]
    struct RecordingServices {
        queried_ctx: std::sync::Mutex<Option<FirmContext>>,
        queried_cap: std::sync::Mutex<Option<String>>,
        emitted: std::sync::Mutex<Vec<Event>>,
    }

    impl HostServices for RecordingServices {
        fn query_graph(
            &self,
            ctx: &FirmContext,
            cap: Option<&str>,
            _query: &GraphQuery,
        ) -> std::result::Result<kanbrick_core::abi::GraphRows, crate::services::HostServicesError>
        {
            *self.queried_ctx.lock().unwrap() = Some(ctx.clone());
            *self.queried_cap.lock().unwrap() = cap.map(str::to_string);
            Ok(kanbrick_core::abi::GraphRows::new(vec![
                serde_json::json!({"ok": true}),
            ]))
        }

        fn emit_event(
            &self,
            _ctx: &FirmContext,
            _cap: Option<&str>,
            event: &Event,
        ) -> std::result::Result<(), crate::services::HostServicesError> {
            self.emitted.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    #[test]
    fn query_import_routes_through_host_services_with_authoritative_ctx() {
        use kanbrick_core::abi::GraphRows;

        let recorder = Arc::new(RecordingServices::default());
        let mut rt = MeshRuntime::new().unwrap().with_services(recorder.clone());
        rt.register_module("q", "0.0.0", QUERY_PROXY_WAT.as_bytes())
            .unwrap();

        let context = ctx("analyst@kanbrick.com", kanbrick_core::ClearanceLevel::L3);
        let query = serde_json::to_vec(&GraphQuery::new("MATCH (n) RETURN n")).unwrap();
        let out = rt.run_with_context("q", &context, &query).unwrap();

        // The guest received exactly the rows the trait returned…
        let rows: GraphRows = serde_json::from_slice(&out).unwrap();
        assert_eq!(rows, GraphRows::new(vec![serde_json::json!({"ok": true})]));
        // …and the trait was handed the host-authoritative caller context, proving
        // the import dispatches through `HostServices` (set via `with_services`),
        // not a bound store.
        assert_eq!(
            recorder.queried_ctx.lock().unwrap().as_ref(),
            Some(&context)
        );
    }

    #[test]
    fn emit_import_routes_through_host_services() {
        let recorder = Arc::new(RecordingServices::default());
        let mut rt = MeshRuntime::new().unwrap().with_services(recorder.clone());
        rt.register_module("e", "0.0.0", EMIT_PROXY_WAT.as_bytes())
            .unwrap();

        let context = ctx("analyst@kanbrick.com", kanbrick_core::ClearanceLevel::L3);
        let event = Event::with_payload("test.kind", serde_json::json!({"x": 1}));
        let input = serde_json::to_vec(&event).unwrap();
        rt.run_with_context("e", &context, &input).unwrap();

        assert_eq!(recorder.emitted.lock().unwrap().as_slice(), &[event]);
    }

    // ---- #70 (Track F): the per-invocation capability threads to HostServices. ----

    /// A guest that ignores its request and queries the graph with an embedded
    /// `GraphQuery` (`{"cypher":"RETURN 1"}`, 21 bytes at offset 0). It exists to
    /// prove that the per-invocation `cap` set on `invoke_with_cap` reaches the
    /// bound [`HostServices`] — the seam the executor's remote backend hangs on.
    const QUERY_CAP_WAT: &str = r#"
        (module
          (import "kanbrick" "kbk_query_graph" (func $query (param i32 i32) (result i64)))
          (memory (export "memory") 1)
          (data (i32.const 0) "{\"cypher\":\"RETURN 1\"}")
          (global $next (mut i32) (i32.const 1024))
          (func (export "kbk_alloc") (param $len i32) (result i32)
            (local $p i32)
            global.get $next
            local.set $p
            global.get $next
            local.get $len
            i32.add
            global.set $next
            local.get $p)
          (func (export "kbk_run") (param $ptr i32) (param $len i32) (result i64)
            i32.const 0
            i32.const 21
            call $query))
    "#;

    #[test]
    fn invoke_with_cap_threads_capability_to_host_services() {
        let recorder = Arc::new(RecordingServices::default());
        let mut rt = MeshRuntime::new().unwrap().with_services(recorder.clone());
        rt.register_module("qcap", "0.0.0", QUERY_CAP_WAT.as_bytes())
            .unwrap();

        let context = ctx("analyst@kanbrick.com", kanbrick_core::ClearanceLevel::L3);
        // The decoded response is irrelevant here (the recorder returns rows that
        // are not a GuestResponse); we only assert the cap + ctx reached the trait.
        let _ = rt.invoke_with_cap(
            "qcap",
            &context,
            Some("cap-token-xyz"),
            &GuestRequest::new(serde_json::Value::Null),
        );
        assert_eq!(
            recorder.queried_cap.lock().unwrap().as_deref(),
            Some("cap-token-xyz"),
            "invoke_with_cap threads the capability to HostServices"
        );
        assert_eq!(
            recorder.queried_ctx.lock().unwrap().as_ref(),
            Some(&context)
        );
    }

    #[test]
    fn plain_invoke_threads_no_capability() {
        let recorder = Arc::new(RecordingServices::default());
        let mut rt = MeshRuntime::new().unwrap().with_services(recorder.clone());
        rt.register_module("qcap", "0.0.0", QUERY_CAP_WAT.as_bytes())
            .unwrap();

        let context = ctx("analyst@kanbrick.com", kanbrick_core::ClearanceLevel::L3);
        let _ = rt.invoke(
            "qcap",
            &context,
            &GuestRequest::new(serde_json::Value::Null),
        );
        assert_eq!(
            recorder.queried_cap.lock().unwrap().as_deref(),
            None,
            "the in-process path passes no capability"
        );
    }
}
