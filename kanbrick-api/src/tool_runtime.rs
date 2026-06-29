//! MCP tool-call runtime seam for the loop run engine (P11.5, ADR-0020).
//!
//! A loop *MCP tool-call step* (P11.5) runs an **external tool** instead of a WASM
//! guest or an LLM completion — the third step kind alongside guest (P11.3) and
//! provider (P11.4). It calls `tachyon-mcp` (the upstream MCP server) wrapped as a
//! **managed sidecar** (probe P8.3), reusing the P7.2 `SidecarSupervisor` pattern —
//! **not** a `kanbrick-mesh::HostServices` backend (that trait is the guest↔host
//! *graph* ABI, a different concern) and **not** a second WASM runtime (ADR-0014).
//!
//! The security-load-bearing rule (ADR-0002/0008, probe P8.3): **the step names only
//! the tool + arguments the `ProjectScope` authorizes — never an identity.** The host
//! mints a per-invocation capability ([`InvocationCaps::mint`](crate::InvocationCaps),
//! bound to the caller's `FirmContext`) and hands the bridge **only** the opaque
//! capability string; the sidecar can neither read nor forge the identity behind it.
//! Identity stays host-side, exactly as the executor split (#70) relays only an opaque
//! cap on graph/event callbacks. This module is the seam where the cap + tool + args
//! meet a bridge implementation:
//!
//! * [`McpBridge`] calls a tool under a minted capability. The run engine mints the
//!   cap, calls [`McpBridge::call_tool`], and revokes the cap the moment it returns.
//! * The default [`StubMcpBridge`] is a no-network echo — the slice ships **no live
//!   subprocess or socket** in core/CI, matching the ADR-0017 / P9.4 / P9.6 / P11.4
//!   discipline. At deploy the real bridge spawns + health-gates the managed
//!   `tachyon-mcp` sidecar (P7.2 `SidecarSupervisor`) and relays over the
//!   `x-kanbrick-internal-token`-gated control channel ([`crate::internal`]),
//!   injected via [`AppState::with_mcp_bridge`](crate::AppState::with_mcp_bridge).

use serde_json::Value as JsonValue;

/// Calls an external MCP tool on behalf of a loop *MCP tool-call step* (P11.5).
///
/// The run engine resolves a step's authorization (`authorize_skill`), mints a
/// per-invocation capability bound to the caller's identity, and calls
/// [`call_tool`](McpBridge::call_tool) with **only** the opaque `cap`, the specific
/// `tool`, and the `args` the scope authorizes. A bridge never receives the caller's
/// identity bytes — it relays the opaque cap to the sidecar, which calls back into the
/// host (resolving the cap server-side) for any authorized work. A step can therefore
/// neither supply its own identity nor act as another caller (ADR-0002, probe P8.3).
pub trait McpBridge: Send + Sync {
    /// Call `tool` with `args` under the per-invocation `cap`. Returns the tool's JSON
    /// result (piped into the next step), or an error string the run engine records as
    /// a step failure. The `cap` is the host-minted, caller-bound capability; the
    /// bridge treats it as opaque.
    fn call_tool(&self, cap: &str, tool: &str, args: &JsonValue) -> Result<JsonValue, String>;
}

/// The default, no-network bridge: returns a canned echo envelope. Ships in place of
/// a live `tachyon-mcp` sidecar (3) so MCP tool steps are exercised end to end with
/// zero egress; the real managed-sidecar bridge is injected at deploy.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubMcpBridge;

impl McpBridge for StubMcpBridge {
    fn call_tool(&self, _cap: &str, tool: &str, args: &JsonValue) -> Result<JsonValue, String> {
        // No subprocess, no socket, no identity: a canned echo so the seam runs in
        // core/CI. A real bridge relays (cap, tool, args) to the managed tachyon-mcp
        // sidecar over the internal-token-gated channel and returns its result; the
        // cap stays opaque to the sidecar, which re-enters the host to resolve it.
        Ok(serde_json::json!({ "tool": tool, "echoed": args }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_echoes_the_tool_and_args_without_touching_the_cap() {
        let bridge = StubMcpBridge;
        let args = serde_json::json!({ "query": "kanbrick", "input": "summary" });
        // The cap is opaque to the stub: any string is accepted and ignored.
        let out = bridge.call_tool("opaque-cap", "web.search", &args).unwrap();
        assert_eq!(out["tool"], "web.search");
        assert_eq!(out["echoed"], args);
    }
}
