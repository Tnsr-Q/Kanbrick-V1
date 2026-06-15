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
use std::time::Duration;

use kanbrick_core::abi::{GuestRequest, GuestResponse};
use kanbrick_core::FirmContext;
use wasmtime::{
    Caller, Config, Engine, Extern, Instance, Linker, Module, Store, StoreLimits,
    StoreLimitsBuilder,
};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

use crate::error::{MeshError, Result};

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

/// Store-local host state: the guest's WASI context, its memory limiter, and the
/// host-authoritative [`FirmContext`] (JSON-encoded) the `kbk_ctx_*` imports
/// expose to the guest (#23). `ctx_json` is empty for the raw [`MeshRuntime::dispatch`]
/// path, which wires no context imports.
struct HostState {
    wasi: WasiP1Ctx,
    limits: StoreLimits,
    ctx_json: Vec<u8>,
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
pub struct MeshRuntime {
    engine: Engine,
    limits: RuntimeLimits,
    registry: HashMap<String, RegisteredGuest>,
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
            registry: HashMap::new(),
        })
    }

    /// The sandbox limits applied to every dispatch.
    pub fn limits(&self) -> &RuntimeLimits {
        &self.limits
    }

    /// Compile `wasm` and register it under `name` with `version`.
    ///
    /// Re-registering an existing name replaces it (the basis for hot-reload, #29).
    pub fn register_module(&mut self, name: &str, version: &str, wasm: &[u8]) -> Result<()> {
        let module = Module::new(&self.engine, wasm).map_err(|e| MeshError::Compile {
            name: name.to_string(),
            detail: e.to_string(),
        })?;
        self.registry.insert(
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
        self.registry.contains_key(name)
    }

    /// Run guest `name` against raw `input` bytes, returning the bytes it
    /// produces. This is the low-level #21 substrate with **no** host context
    /// or imports; see [`invoke`](Self::invoke) for the typed, context-bearing
    /// entry point.
    ///
    /// A fresh, sandboxed instance is created for the call and dropped afterward.
    pub fn dispatch(&self, name: &str, input: &[u8]) -> Result<Vec<u8>> {
        let module = self.module(name)?;
        let mut store = self.new_store(Vec::new())?;
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
        let input = request.to_json_bytes().map_err(|e| MeshError::BadOutput {
            name: name.to_string(),
            detail: format!("encoding request: {e}"),
        })?;
        let output = self.run_with_context(name, ctx, &input)?;
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
        let ctx_json = serde_json::to_vec(ctx)
            .map_err(|e| MeshError::Engine(format!("serializing firm context: {e}")))?;
        let module = self.module(name)?;
        let mut store = self.new_store(ctx_json)?;
        let mut linker: Linker<HostState> = Linker::new(&self.engine);
        self.add_wasi(&mut linker)?;
        self.add_context_imports(&mut linker)?;
        let instance = self.instantiate_guest(&mut store, name, &module, &linker)?;
        self.call_run(&mut store, &instance, name, input)
    }

    /// Clone a registered guest's compiled module (cheap — `Module` is `Arc`-backed).
    fn module(&self, name: &str) -> Result<Module> {
        self.registry
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
                    let memory = match caller.get_export("memory") {
                        Some(Extern::Memory(m)) => m,
                        _ => return Err(wasmtime::Error::msg("guest has no exported memory")),
                    };
                    memory.write(&mut caller, ptr as usize, &json)?;
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
            .map_err(|e| MeshError::Trap {
                name: name.to_string(),
                detail: format!("kbk_alloc: {e}"),
            })?;
        memory
            .write(&mut *store, in_ptr as usize, input)
            .map_err(|e| MeshError::BadOutput {
                name: name.to_string(),
                detail: format!("writing input: {e}"),
            })?;

        let packed = run
            .call(&mut *store, (in_ptr, in_len))
            .map_err(|e| MeshError::Trap {
                name: name.to_string(),
                detail: format!("kbk_run: {e}"),
            })?;

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
    /// (possibly empty) host-authoritative context JSON.
    fn new_store(&self, ctx_json: Vec<u8>) -> Result<Store<HostState>> {
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
            },
        );
        store.limiter(|s| &mut s.limits);
        store
            .set_fuel(self.limits.fuel)
            .map_err(|e| MeshError::Engine(e.to_string()))?;
        // No epoch ticker runs in this slice, so set a deadline that never fires;
        // the scheduler (#25) provisions a real timeout.
        store.set_epoch_deadline(u64::MAX);
        Ok(store)
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
