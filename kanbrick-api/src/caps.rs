//! Per-invocation capability registry (#69, Track E).
//!
//! When a guest runs on an executor (#70) and calls back to the control plane to
//! read the graph or emit an event, that callback must execute under the caller's
//! **real** clearance — and a compromised executor (or a WASM escape) must not be
//! able to act as a different identity. The control plane therefore mints a
//! short-lived **capability** per invocation, bound here (server-side) to the
//! authoritative [`FirmContext`]. The executor only ever relays the opaque
//! capability string; it can neither read nor forge the identity behind it.
//!
//! Because the control plane is single-replica, this in-memory map is the
//! authoritative store — and the `FirmContext` bytes never leave the process.
//! This extends ADR-0002's host-authoritative identity across the network hop.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use kanbrick_core::FirmContext;
use uuid::Uuid;

/// A minted capability bound to one invocation's authoritative context.
struct CapEntry {
    /// The host-authoritative caller identity this capability stands in for.
    ctx: FirmContext,
    /// When this capability stops resolving.
    expires_at: Instant,
}

/// A registry of live per-invocation capabilities.
///
/// A capability is a bearer token: unguessable, single-invocation, and
/// short-lived. It is generated from the OS CSPRNG (two v4 UUIDs ⇒ 244 bits of
/// entropy, hyphen-free hex) so it cannot be guessed within its TTL.
#[derive(Default)]
pub struct InvocationCaps {
    entries: RwLock<HashMap<String, CapEntry>>,
}

impl InvocationCaps {
    /// An empty registry.
    pub fn new() -> Self {
        InvocationCaps::default()
    }

    /// Mint a capability bound to `ctx`, valid for `ttl`. Returns the opaque
    /// capability string to hand to the executor. Opportunistically sweeps
    /// expired entries so abandoned invocations don't leak memory.
    pub fn mint(&self, ctx: FirmContext, ttl: Duration) -> String {
        let cap = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let expires_at = Instant::now() + ttl;
        let mut entries = self.entries.write().expect("caps lock");
        entries.retain(|_, e| e.expires_at > Instant::now());
        entries.insert(cap.clone(), CapEntry { ctx, expires_at });
        cap
    }

    /// Resolve `cap` to its bound [`FirmContext`], or `None` if it is unknown or
    /// expired. This is the only way the internal callbacks learn the identity to
    /// run under — the executor never names it.
    pub fn resolve(&self, cap: &str) -> Option<FirmContext> {
        let now = Instant::now();
        let entries = self.entries.read().expect("caps lock");
        match entries.get(cap) {
            Some(entry) if entry.expires_at > now => Some(entry.ctx.clone()),
            _ => None,
        }
    }

    /// Drop a capability once its invocation completes. Idempotent.
    pub fn revoke(&self, cap: &str) {
        self.entries.write().expect("caps lock").remove(cap);
    }

    /// Number of live (not yet swept) capabilities. Test/diagnostic helper.
    #[cfg(test)]
    pub fn live_count(&self) -> usize {
        self.entries.read().expect("caps lock").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanbrick_core::ClearanceLevel;

    fn ctx() -> FirmContext {
        FirmContext::new(Uuid::new_v4(), "analyst@kanbrick.com", ClearanceLevel::L3)
    }

    #[test]
    fn mint_then_resolve_round_trips_the_context() {
        let caps = InvocationCaps::new();
        let c = ctx();
        let cap = caps.mint(c.clone(), Duration::from_secs(60));
        assert_eq!(caps.resolve(&cap), Some(c));
        // The token is unguessable-length hex (two hyphen-free UUIDs).
        assert_eq!(cap.len(), 64);
        assert!(cap.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn unknown_capability_does_not_resolve() {
        let caps = InvocationCaps::new();
        assert_eq!(caps.resolve("deadbeef"), None);
    }

    #[test]
    fn revoked_capability_does_not_resolve() {
        let caps = InvocationCaps::new();
        let cap = caps.mint(ctx(), Duration::from_secs(60));
        caps.revoke(&cap);
        assert_eq!(caps.resolve(&cap), None);
        // Revoking an unknown capability is a no-op.
        caps.revoke("nope");
    }

    #[test]
    fn expired_capability_does_not_resolve() {
        let caps = InvocationCaps::new();
        let cap = caps.mint(ctx(), Duration::from_millis(5));
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(caps.resolve(&cap), None, "capability has expired");
    }

    #[test]
    fn minting_sweeps_expired_entries() {
        let caps = InvocationCaps::new();
        caps.mint(ctx(), Duration::from_millis(5));
        std::thread::sleep(Duration::from_millis(20));
        // Minting a fresh capability sweeps the expired one, so only the new
        // entry remains.
        let _live = caps.mint(ctx(), Duration::from_secs(60));
        assert_eq!(caps.live_count(), 1);
    }
}
