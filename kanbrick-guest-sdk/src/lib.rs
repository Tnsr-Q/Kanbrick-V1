//! # kanbrick-guest-sdk
//!
//! Typed bindings to the Kanbrick host ABI for WASM guest modules (issue #39).
//!
//! A guest links this crate and writes a single handler
//! `fn(GuestRequest) -> Result<GuestResponse>`, then wires the entrypoint with
//! [`guest_entrypoint!`]. The SDK provides the ergonomic capability surface the
//! host exposes (ADR-0002 / #22–#24):
//!
//! * [`firm_context`] — the caller's host-authoritative [`FirmContext`] (#23).
//! * [`query_graph`] — run a clearance-filtered [`GraphQuery`] (#24).
//! * [`emit`] — publish an [`Event`] onto the host event bus (#27).
//! * [`log`] — record a log line at a [`LogLevel`].
//!
//! All payloads cross the boundary as JSON (ADR-0002), reusing the **shared**
//! [`kanbrick_core::abi`] types — host and guest can never disagree on the wire
//! shape because they are the same types.
//!
//! ## Targets
//!
//! The host calls and the `kbk_*` imports only exist inside a `wasm32` guest. On
//! the host target the capability functions are present (so the crate compiles,
//! lints, and can be depended on by native code) but `unimplemented!()` — a
//! guest's *pure logic* is meant to be unit-tested natively without them, and the
//! glue that calls them runs only in WASM. See `guests/sdk-example`.
//!
//! ## Example
//!
//! ```ignore
//! use kanbrick_guest_sdk as sdk;
//! use sdk::{GuestRequest, GuestResponse, GraphQuery, Result};
//!
//! fn handle(_req: GuestRequest) -> Result<GuestResponse> {
//!     let ctx = sdk::firm_context()?;
//!     let rows = sdk::query_graph(&GraphQuery::new("MATCH (c:Company) RETURN c.company_id"))?;
//!     sdk::log(sdk::LogLevel::Info, "counted companies");
//!     Ok(GuestResponse::new(serde_json::json!({
//!         "caller": ctx.email,
//!         "companies": rows.len(),
//!     })))
//! }
//!
//! sdk::guest_entrypoint!(handle);
//! ```

// Re-export the shared ABI surface so a guest depends only on this crate.
pub use kanbrick_core::abi::{Event, GraphQuery, GraphRows, GuestRequest, GuestResponse, LogLevel};
pub use kanbrick_core::{ClearanceLevel, Error, ErrorKind, FirmContext, Result};
// Re-exported so a guest can build JSON payloads without its own serde_json dep.
pub use serde_json;

/// Raw host imports, published by the runtime under the `"kanbrick"` module.
#[cfg(target_arch = "wasm32")]
mod imports {
    #[link(wasm_import_module = "kanbrick")]
    extern "C" {
        /// Length, in bytes, of the caller's `FirmContext` JSON (#23).
        pub fn kbk_ctx_len() -> u32;
        /// Write the caller's `FirmContext` JSON into guest memory at `ptr` (#23).
        pub fn kbk_ctx_read(ptr: u32);
        /// Run the `GraphQuery` JSON at `[in_ptr, in_ptr+in_len)` and return the
        /// packed `(out_ptr << 32) | out_len` of the `GraphRows` JSON the host
        /// wrote back into guest memory (#24).
        pub fn kbk_query_graph(in_ptr: u32, in_len: u32) -> u64;
        /// Publish the `Event` JSON at `[in_ptr, in_ptr+in_len)` (#27).
        pub fn kbk_emit_event(in_ptr: u32, in_len: u32);
        /// Log `[in_ptr, in_ptr+in_len)` (UTF-8) at the given `level` code.
        pub fn kbk_log(level: u32, in_ptr: u32, in_len: u32);
    }
}

/// Numeric wire code for a [`LogLevel`] (mirrors the host's `kbk_log` decoder).
fn level_code(level: LogLevel) -> u32 {
    match level {
        LogLevel::Error => 0,
        LogLevel::Warn => 1,
        LogLevel::Info => 2,
        LogLevel::Debug => 3,
        LogLevel::Trace => 4,
    }
}

/// Reserve `len` bytes of guest linear memory and return the offset.
///
/// Exported as the guest's `kbk_alloc` by [`guest_entrypoint!`]. The host writes
/// query results and inputs here; the buffers are intentionally leaked and
/// reclaimed wholesale when the per-dispatch instance is torn down (ADR-0002).
#[doc(hidden)]
pub fn alloc(len: u32) -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        let mut buf: Vec<u8> = Vec::with_capacity(len as usize);
        let ptr = buf.as_mut_ptr() as u32;
        core::mem::forget(buf);
        ptr
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = len;
        unimplemented!("kbk_alloc is only callable inside a wasm32 guest")
    }
}

/// The caller's host-authoritative [`FirmContext`] for this invocation (#23).
///
/// Identity is supplied by the host and can never be set or forged by the guest.
pub fn firm_context() -> Result<FirmContext> {
    #[cfg(target_arch = "wasm32")]
    {
        let len = unsafe { imports::kbk_ctx_len() } as usize;
        let mut buf = vec![0u8; len];
        unsafe { imports::kbk_ctx_read(buf.as_mut_ptr() as u32) };
        serde_json::from_slice(&buf)
            .map_err(|e| Error::Internal(format!("decoding firm context: {e}")))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        unimplemented!("firm_context is only callable inside a wasm32 guest")
    }
}

/// Run `query` under the caller's clearance and return the rows they may see.
///
/// The host routes every query through the clearance-enforcing `GuardedStore`
/// (#24), so a guest only ever receives data its caller is permitted to read.
pub fn query_graph(query: &GraphQuery) -> Result<GraphRows> {
    #[cfg(target_arch = "wasm32")]
    {
        let input = serde_json::to_vec(query)
            .map_err(|e| Error::Internal(format!("encoding query: {e}")))?;
        let packed = unsafe { imports::kbk_query_graph(input.as_ptr() as u32, input.len() as u32) };
        let out = unsafe { read_packed(packed) };
        serde_json::from_slice(&out).map_err(|e| Error::Internal(format!("decoding rows: {e}")))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = query;
        unimplemented!("query_graph is only callable inside a wasm32 guest")
    }
}

/// Publish `event` onto the host event bus (#27).
pub fn emit(event: &Event) -> Result<()> {
    #[cfg(target_arch = "wasm32")]
    {
        let input = serde_json::to_vec(event)
            .map_err(|e| Error::Internal(format!("encoding event: {e}")))?;
        unsafe { imports::kbk_emit_event(input.as_ptr() as u32, input.len() as u32) };
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = event;
        unimplemented!("emit is only callable inside a wasm32 guest")
    }
}

/// Record a log line at `level`.
pub fn log(level: LogLevel, message: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        unsafe {
            imports::kbk_log(
                level_code(level),
                message.as_ptr() as u32,
                message.len() as u32,
            )
        };
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (level_code(level), message);
    }
}

/// Read the bytes a host import wrote back, from a packed `(ptr << 32) | len`.
#[cfg(target_arch = "wasm32")]
unsafe fn read_packed(packed: u64) -> Vec<u8> {
    let ptr = ((packed >> 32) as u32) as *const u8;
    let len = (packed & 0xffff_ffff) as usize;
    core::slice::from_raw_parts(ptr, len).to_vec()
}

/// Drive one dispatch: decode the input [`GuestRequest`], run `handler`, and
/// encode the [`GuestResponse`] into freshly [`alloc`]ated guest memory, returning
/// the packed `(ptr << 32) | len`. A handler error becomes a **structured error
/// response** (never a panic), per the PRD's "malformed input → structured error".
///
/// Wired as the guest's `kbk_run` by [`guest_entrypoint!`].
#[doc(hidden)]
pub fn run<F>(in_ptr: u32, in_len: u32, handler: F) -> u64
where
    F: FnOnce(GuestRequest) -> Result<GuestResponse>,
{
    #[cfg(target_arch = "wasm32")]
    {
        let input = unsafe { core::slice::from_raw_parts(in_ptr as *const u8, in_len as usize) };
        let response = match GuestRequest::from_json_bytes(input) {
            Ok(request) => match handler(request) {
                Ok(response) => response,
                Err(e) => error_response(&e),
            },
            Err(e) => error_response(&e),
        };
        let bytes = response
            .to_json_bytes()
            .unwrap_or_else(|_| br#"{"payload":{"error":"response encode failed"}}"#.to_vec());
        let out_ptr = alloc(bytes.len() as u32);
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out_ptr as *mut u8, bytes.len()) };
        ((out_ptr as u64) << 32) | (bytes.len() as u64 & 0xffff_ffff)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (in_ptr, in_len, handler);
        unimplemented!("kbk_run is only callable inside a wasm32 guest")
    }
}

/// Build a structured error [`GuestResponse`] from an [`Error`] — the uniform way
/// a guest surfaces a failure to its caller without trapping.
pub fn error_response(error: &Error) -> GuestResponse {
    GuestResponse::new(serde_json::json!({
        "error": error.to_string(),
        "kind": format!("{:?}", error.kind()),
    }))
}

/// Wire a guest's WASM entrypoint to a handler `fn(GuestRequest) -> Result<GuestResponse>`.
///
/// Generates the exported `kbk_alloc` and `kbk_run` (the ADR-0002 calling
/// convention). Only emitted for `wasm32`, so a guest crate still builds and
/// unit-tests its pure logic natively.
#[macro_export]
macro_rules! guest_entrypoint {
    ($handler:path) => {
        #[cfg(target_arch = "wasm32")]
        #[no_mangle]
        pub extern "C" fn kbk_alloc(len: u32) -> u32 {
            $crate::alloc(len)
        }

        #[cfg(target_arch = "wasm32")]
        #[no_mangle]
        pub extern "C" fn kbk_run(ptr: u32, len: u32) -> u64 {
            $crate::run(ptr, len, $handler)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_codes_are_stable() {
        assert_eq!(level_code(LogLevel::Error), 0);
        assert_eq!(level_code(LogLevel::Warn), 1);
        assert_eq!(level_code(LogLevel::Info), 2);
        assert_eq!(level_code(LogLevel::Debug), 3);
        assert_eq!(level_code(LogLevel::Trace), 4);
    }

    #[test]
    fn error_response_carries_message_and_kind() {
        let resp = error_response(&Error::AccessDenied {
            required: ClearanceLevel::L4,
            actual: ClearanceLevel::L2,
        });
        assert_eq!(resp.payload["kind"], "Unauthorized");
        assert!(resp.payload["error"]
            .as_str()
            .unwrap()
            .contains("requires clearance L4"));
    }

    #[test]
    fn shared_abi_types_are_reexported() {
        // A guest constructs requests/queries entirely through this crate.
        let q = GraphQuery::new("MATCH (c:Company) RETURN c.company_id").param("x", 1);
        assert_eq!(q.params.len(), 1);
        let req = GuestRequest::new(serde_json::json!({"company_id": "JMTS"}));
        assert_eq!(req.payload["company_id"], "JMTS");
    }
}
