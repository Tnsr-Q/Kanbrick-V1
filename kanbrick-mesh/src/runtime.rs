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
use std::sync::{Arc, RwLock};
use std::time::Duration;

use kanbrick_auth::GuardedStore;
use kanbrick_core::abi::{Event, GraphQuery, GraphRows, GuestRequest, GuestResponse};
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

/// The import module name the host functions are published under; guests declare
/// `#[link(wasm_import_module = "kanbrick")]` to use them.
const HOST_MODULE: &str = "kanbrick";

/// What the `kbk_query_graph` import needs to service a guest query: the firm
/// graph plus the caller's host-authoritative context, routed through the
/// clearance-enforcing [`GuardedStore`] (#24).
struct QueryBackend {
    store: Arc<GraphStore>,
    ctx: FirmContext,
}

impl QueryBackend {
    /// Run `query` under the caller's clearance, returning JSON-encoded
    /// [`GraphRows`]. Errors surface as a guest trap.
    fn run(&self, query: &GraphQuery) -> wasmtime::Result<Vec<u8>> {
        let guarded = GuardedStore::new(&self.store, &self.ctx)
            .map_err(|e| wasmtime::Error::msg(e.to_string()))?;
        let rows: GraphRows = guarded
            .query_graph(query)
            .map_err(|e| wasmtime::Error::msg(e.to_string()))?;
        serde_json::to_vec(&rows).map_err(|e| wasmtime::Error::msg(e.to_string()))
    }
}

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

/// Store-local host state: the guest's WASI context, its memory limiter, the
/// host-authoritative [`FirmContext`] (JSON-encoded) the `kbk_ctx_*` imports
/// expose to the guest (#23), and the optional graph-query backend the
/// `kbk_query_graph` import uses (#24). `ctx_json` is empty and `query` is `None`
/// for the raw [`MeshRuntime::dispatch`] path, which wires no host imports.
struct HostState {
    wasi: WasiP1Ctx,
    limits: StoreLimits,
    ctx_json: Vec<u8>,
    query: Option<QueryBackend>,
    bus: Option<EventBus>,
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
    store: Option<Arc<GraphStore>>,
    bus: Option<EventBus>,
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
            store: None,
            bus: None,
        })
    }

    /// Bind the firm graph so guests' `query_graph` calls can run (#24). Without
    /// a bound store, a `kbk_query_graph` call traps. Returns `self` for chaining.
    pub fn with_store(mut self, store: Arc<GraphStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Bind an [`EventBus`] so guests' `emit` calls publish onto it (#27/#46).
    /// Without a bound bus, an emitted event is logged and dropped. Builder-style.
    pub fn with_bus(mut self, bus: EventBus) -> Self {
        self.bus = Some(bus);
        self
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
        let mut store = self.new_store(Vec::new(), None, None, u64::MAX)?;
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
        self.invoke_with_deadline(name, ctx, request, u64::MAX)
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
        let input = request.to_json_bytes().map_err(|e| MeshError::BadOutput {
            name: name.to_string(),
            detail: format!("encoding request: {e}"),
        })?;
        let output = self.run_with_context_deadline(name, ctx, &input, epoch_deadline)?;
        GuestResponse::from_json_bytes(&output).map_err(|e| MeshError::BadOutput {
            name: name.to_string(),
            detail: format!("decoding response: {e}"),
        })
    }

    /// Run guest `name` against raw `input`, injecting `ctx` as the
    /// host-authoritative [`FirmContext`] readable through the `kbk_ctx_*` host
    /// imports. Returns the raw output bytes. ([`invoke`](Self::invoke) is the
    /// typed wrapper.)
    pub fn run_with_context(&self, name: &str, ctx: &FirmContext, input: &[u8]) -> Result<Vec<u8>> {
        self.run_with_context_deadline(name, ctx, input, u64::MAX)
    }

    /// [`run_with_context`](Self::run_with_context) with an explicit epoch
    /// deadline (`u64::MAX` = unbounded).
    fn run_with_context_deadline(
        &self,
        name: &str,
        ctx: &FirmContext,
        input: &[u8],
        epoch_deadline: u64,
    ) -> Result<Vec<u8>> {
        let ctx_json = serde_json::to_vec(ctx)
            .map_err(|e| MeshError::Engine(format!("serializing firm context: {e}")))?;
        let module = self.module(name)?;
        let backend = self.store.clone().map(|store| QueryBackend {
            store,
            ctx: ctx.clone(),
        });
        let mut store = self.new_store(ctx_json, backend, self.bus.clone(), epoch_deadline)?;
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
    /// which the host services by running the query through [`GuardedStore`] and
    /// returning the packed `(out_ptr, out_len)` of the JSON [`GraphRows`] written
    /// back into guest memory.
    ///
    /// This import is re-entrant: to return a variable-length result the host
    /// calls back into the guest's own `kbk_alloc`. If no store is bound, the call
    /// traps (a guest cannot query a runtime with no graph).
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

                    // Run it through the clearance-enforcing backend.
                    let backend =
                        caller.data().query.as_ref().ok_or_else(|| {
                            wasmtime::Error::msg("no graph bound to this runtime")
                        })?;
                    // SAFETY of borrows: `run` does not touch `caller`, so take the
                    // result bytes before reborrowing `caller` mutably below.
                    let out = backend.run(&query)?;

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
    /// the [`Event`] JSON the guest wrote and publishes it onto the bound
    /// [`EventBus`]. With no bus bound the event is logged and dropped (never
    /// retained), matching the host-side [`MeshHost`](crate::MeshHost) behaviour.
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
                    match caller.data().bus.as_ref() {
                        Some(bus) => {
                            bus.emit(event);
                        }
                        None => tracing::info!(
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
    /// (possibly empty) host-authoritative context JSON and optional query
    /// backend, and armed with `epoch_deadline` engine ticks before it is killed
    /// (`u64::MAX` = no wall-clock limit). The [`Scheduler`](crate::Scheduler)
    /// drives the engine epoch that makes a finite deadline fire (#25).
    fn new_store(
        &self,
        ctx_json: Vec<u8>,
        query: Option<QueryBackend>,
        bus: Option<EventBus>,
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
                query,
                bus,
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
}
