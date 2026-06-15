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

use wasmtime::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

use crate::error::{MeshError, Result};

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

/// Store-local host state: the guest's WASI context plus its memory limiter.
struct HostState {
    wasi: WasiP1Ctx,
    limits: StoreLimits,
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

    /// Run guest `name` against `input`, returning the bytes it produces.
    ///
    /// A fresh, sandboxed instance is created for the call and dropped afterward.
    pub fn dispatch(&self, name: &str, input: &[u8]) -> Result<Vec<u8>> {
        let guest = self
            .registry
            .get(name)
            .ok_or_else(|| MeshError::GuestNotFound(name.to_string()))?;

        let mut store = self.new_store()?;
        let mut linker: Linker<HostState> = Linker::new(&self.engine);
        wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |s: &mut HostState| &mut s.wasi)
            .map_err(|e| MeshError::Link(e.to_string()))?;

        let instance =
            linker
                .instantiate(&mut store, &guest.module)
                .map_err(|e| MeshError::Instantiate {
                    name: name.to_string(),
                    detail: e.to_string(),
                })?;

        // Reactor guests (cdylib) export `_initialize`; run it once if present.
        if let Ok(init) = instance.get_typed_func::<(), ()>(&mut store, "_initialize") {
            init.call(&mut store, ()).map_err(|e| MeshError::Trap {
                name: name.to_string(),
                detail: format!("_initialize: {e}"),
            })?;
        }

        let memory =
            instance
                .get_memory(&mut store, "memory")
                .ok_or_else(|| MeshError::MissingExport {
                    name: name.to_string(),
                    export: "memory".to_string(),
                    detail: "not exported".to_string(),
                })?;
        let alloc = instance
            .get_typed_func::<u32, u32>(&mut store, "kbk_alloc")
            .map_err(|e| MeshError::MissingExport {
                name: name.to_string(),
                export: "kbk_alloc".to_string(),
                detail: e.to_string(),
            })?;
        let run = instance
            .get_typed_func::<(u32, u32), u64>(&mut store, "kbk_run")
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
            .call(&mut store, in_len)
            .map_err(|e| MeshError::Trap {
                name: name.to_string(),
                detail: format!("kbk_alloc: {e}"),
            })?;
        memory
            .write(&mut store, in_ptr as usize, input)
            .map_err(|e| MeshError::BadOutput {
                name: name.to_string(),
                detail: format!("writing input: {e}"),
            })?;

        let packed = run
            .call(&mut store, (in_ptr, in_len))
            .map_err(|e| MeshError::Trap {
                name: name.to_string(),
                detail: format!("kbk_run: {e}"),
            })?;

        let out_ptr = (packed >> 32) as usize;
        let out_len = (packed & 0xffff_ffff) as usize;
        let mut out = vec![0u8; out_len];
        memory
            .read(&store, out_ptr, &mut out)
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

    /// Build a fresh, sandboxed [`Store`] for a single dispatch.
    fn new_store(&self) -> Result<Store<HostState>> {
        // Locked-down WASI: no preopened dirs, no network, no inherited stdio.
        let wasi = WasiCtxBuilder::new().build_p1();
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.limits.max_memory_bytes)
            .build();
        let mut store = Store::new(&self.engine, HostState { wasi, limits });
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
}
