//! The BYO-AI **egress gate** at the `kanbrick-providers` send boundary (P9.6, #106).
//!
//! ADR-0017 makes this the **one place** core data may leave the system: the core
//! stays no-egress, and an outbound provider call is permitted only if **all** of
//! three additive-only checks pass — every one can *restrict*, none can elevate:
//!
//! 1. **Restrict-only RBAC** (ADR-0010, ported from `probes/rbac-overlay`). A role
//!    tag maps to an optional clearance *ceiling*; effective clearance is the base
//!    lowered by `min` over every applicable ceiling, so a role can only ever
//!    narrow it. Reads the firm's existing `FirmContext.roles` — **no second role
//!    store**. The egress must still meet the data class's clearance floor.
//! 2. **Per-tenant host allowlist** (ADR-0017), **default-deny**: an un-allowlisted
//!    provider host is refused *before any socket opens*.
//! 3. **DLP** (ADR-0010), **default-deny**: the `(data-class → provider)` pair must
//!    be explicitly allowed — orthogonal to clearance, so a disallowed pair is
//!    blocked even at L5.
//!
//! Every allow **and** deny is audited (the [`EgressAuditSink`]). The gate is
//! applied by [`GatedTransport`], a decorator over the P9.2
//! [`HttpTransport`](kanbrick_providers::wire::HttpTransport): a denied call never
//! reaches the inner transport, so no socket is opened. The inner transport is
//! injected — the real TLS `reqwest` client at deploy time, or an in-test stub —
//! so this crate carries **no HTTP/TLS stack** and is fully offline-testable
//! (matching #106's stub-based verification plan).
//!
//! **Deployment backstop.** The app-layer gate here is the *authority*; ADR-0017's
//! Kubernetes **NetworkPolicy** (egress-allow only the providers pod → the
//! allowlisted hosts `:443`, core pods egress-denied) is defense-in-depth, added to
//! `deploy/k8s/` when a CP is deployed. The network policy cannot reason about data
//! class — that is why the DLP gate must be the authority.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use kanbrick_core::ClearanceLevel;
use kanbrick_providers::wire::{HttpRequest, HttpResponse, HttpTransport};
use kanbrick_providers::{ProviderError, ProviderKind};
use serde::Serialize;

/// The sensitivity class of whatever is about to be sent to a provider.
///
/// In production this is derived from the source `ProjectScope` + graph labels
/// (ADR-0005/0010); here it is an explicit tag carried in the [`EgressContext`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DataClass {
    /// Public roster / already-public company facts (ADR-0005).
    Public,
    /// Internal firm data.
    Internal,
    /// Restricted / sensitive (L4+ material, PII, deal terms).
    Restricted,
}

impl DataClass {
    /// The clearance floor an egress of this class must meet, *after* the role
    /// overlay has narrowed the caller's clearance.
    pub fn min_clearance(self) -> ClearanceLevel {
        match self {
            DataClass::Public => ClearanceLevel::L1,
            DataClass::Internal => ClearanceLevel::L2,
            DataClass::Restricted => ClearanceLevel::L4,
        }
    }
}

/// Restrict-only RBAC overlay (ADR-0010): role tag → optional clearance ceiling.
///
/// Reads role tags exactly as they appear in `FirmContext.roles`. A role absent
/// from the map imposes no ceiling (it cannot grant anything); a present role
/// imposes a ceiling that can only *lower* the effective clearance.
#[derive(Debug, Default, Clone)]
pub struct RoleOverlay {
    ceilings: HashMap<String, ClearanceLevel>,
}

impl RoleOverlay {
    /// An overlay with no ceilings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a role that *caps* clearance at `ceiling` while held (e.g.
    /// `"contractor" → L2`, `"dlp_quarantine" → L1`).
    pub fn with_ceiling(mut self, role: impl Into<String>, ceiling: ClearanceLevel) -> Self {
        self.ceilings.insert(role.into(), ceiling);
        self
    }

    /// Effective clearance after applying every applicable ceiling to `base`.
    /// **Monotonically non-increasing in the role set** — adding a role can only
    /// lower (or leave) the result, never raise it (`min` is the whole safety
    /// argument).
    pub fn effective_clearance(&self, base: ClearanceLevel, roles: &[String]) -> ClearanceLevel {
        let mut effective = base;
        for role in roles {
            if let Some(&ceiling) = self.ceilings.get(role) {
                effective = effective.min(ceiling);
            }
        }
        effective
    }
}

/// Default-deny DLP allowlist of `(data-class → provider)` pairs (ADR-0010).
#[derive(Debug, Default, Clone)]
pub struct DlpPolicy {
    allowed: HashSet<(DataClass, ProviderKind)>,
}

impl DlpPolicy {
    /// An empty (deny-all) policy.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow a specific `(data-class → provider)` egress pair (builder).
    pub fn allow(mut self, class: DataClass, provider: ProviderKind) -> Self {
        self.allowed.insert((class, provider));
        self
    }

    /// Default-deny: a pair is sendable only if explicitly allowlisted.
    pub fn can_send(&self, class: DataClass, provider: ProviderKind) -> bool {
        self.allowed.contains(&(class, provider))
    }
}

/// Per-tenant **default-deny** allowlist of provider hosts (ADR-0017). An
/// un-allowlisted host is refused before any socket opens.
#[derive(Debug, Default, Clone)]
pub struct HostAllowlist {
    hosts: HashSet<String>,
}

impl HostAllowlist {
    /// An empty (deny-all) allowlist.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow a provider host, e.g. `"api.anthropic.com"` (builder).
    ///
    /// Stored canonical lowercase: hostnames are case-insensitive (DNS), so the
    /// gate must match canonically — a security boundary should neither be fooled
    /// by case nor refuse a legitimate host on case alone.
    pub fn allow(mut self, host: impl Into<String>) -> Self {
        self.hosts.insert(host.into().to_ascii_lowercase());
        self
    }

    /// Whether `host` is on the allowlist (case-insensitive).
    pub fn is_allowed(&self, host: &str) -> bool {
        self.hosts.contains(&host.to_ascii_lowercase())
    }
}

/// Why an egress was refused. Each variant is a *restriction*; none can elevate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EgressDenial {
    /// The caller's effective (post-overlay) clearance is below the data class's floor.
    #[error("egress denied: effective clearance {effective} is below {required} required for {class:?} data")]
    InsufficientClearance {
        /// The floor the data class requires.
        required: ClearanceLevel,
        /// The caller's clearance after the role overlay.
        effective: ClearanceLevel,
        /// The data class being sent.
        class: DataClass,
    },
    /// The provider host is not on the per-tenant allowlist (default-deny).
    #[error("egress denied: provider host {host} is not on the per-tenant allowlist")]
    HostNotAllowlisted {
        /// The refused host.
        host: String,
    },
    /// DLP refuses this `(data-class → provider)` pair (default-deny, orthogonal to clearance).
    #[error("egress denied: DLP blocks {class:?} data → {provider}")]
    DlpBlocked {
        /// The data class.
        class: DataClass,
        /// The provider.
        provider: ProviderKind,
    },
}

/// The combined egress gate: RBAC overlay + host allowlist + DLP.
#[derive(Debug, Default, Clone)]
pub struct EgressGate {
    overlay: RoleOverlay,
    allowlist: HostAllowlist,
    dlp: DlpPolicy,
}

impl EgressGate {
    /// Build a gate from its three policies.
    pub fn new(overlay: RoleOverlay, allowlist: HostAllowlist, dlp: DlpPolicy) -> Self {
        EgressGate {
            overlay,
            allowlist,
            dlp,
        }
    }

    /// Authorize an egress, returning the effective clearance on success.
    ///
    /// Order: narrow clearance by roles → check the data class's floor → host
    /// allowlist (default-deny) → DLP (default-deny). All must pass; any failure is
    /// a [`EgressDenial`] and **no** I/O is performed by the caller.
    pub fn authorize(
        &self,
        base_clearance: ClearanceLevel,
        roles: &[String],
        class: DataClass,
        provider: ProviderKind,
        host: &str,
    ) -> Result<ClearanceLevel, EgressDenial> {
        let effective = self.overlay.effective_clearance(base_clearance, roles);
        let required = class.min_clearance();
        if !effective.satisfies(required) {
            return Err(EgressDenial::InsufficientClearance {
                required,
                effective,
                class,
            });
        }
        if !self.allowlist.is_allowed(host) {
            return Err(EgressDenial::HostNotAllowlisted {
                host: host.to_string(),
            });
        }
        if !self.dlp.can_send(class, provider) {
            return Err(EgressDenial::DlpBlocked { class, provider });
        }
        Ok(effective)
    }
}

/// One audited egress decision. Carries no payload — only the policy-relevant
/// dimensions and the outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EgressRecord {
    /// Whether the egress was allowed.
    pub allowed: bool,
    /// The data class.
    pub class: DataClass,
    /// The target provider.
    pub provider: ProviderKind,
    /// The target host (from the request URL).
    pub host: String,
    /// The denial reason, present iff `!allowed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denial: Option<String>,
}

/// Sink for egress audit records — **every** allow and deny is recorded (#106 AC).
///
/// The production sink wraps `kanbrick_auth::AuditLog` (recording under the caller's
/// identity); tests use [`RecordingAudit`]. Object-safe + `Send + Sync` so a
/// [`GatedTransport`] can hold `Arc<dyn EgressAuditSink>`.
pub trait EgressAuditSink: Send + Sync {
    /// Record one egress decision.
    fn record(&self, record: &EgressRecord);
}

/// An [`EgressAuditSink`] that drops records (audit disabled / not yet wired).
pub struct NoAudit;

impl EgressAuditSink for NoAudit {
    fn record(&self, _record: &EgressRecord) {}
}

/// An in-memory [`EgressAuditSink`] that retains every record — for tests and
/// local diagnostics.
#[derive(Default)]
pub struct RecordingAudit {
    records: Mutex<Vec<EgressRecord>>,
}

impl RecordingAudit {
    /// An empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// A snapshot of the recorded decisions, in order.
    pub fn records(&self) -> Vec<EgressRecord> {
        self.records.lock().expect("egress audit lock").clone()
    }
}

impl EgressAuditSink for RecordingAudit {
    fn record(&self, record: &EgressRecord) {
        self.records
            .lock()
            .expect("egress audit lock")
            .push(record.clone());
    }
}

/// The caller context bound to a [`GatedTransport`] for one call's lifetime: who is
/// sending (clearance + roles) and what (data class + provider).
#[derive(Debug, Clone)]
pub struct EgressContext {
    /// The caller's authenticated base clearance (from `FirmContext`).
    pub clearance: ClearanceLevel,
    /// The caller's role tags (`FirmContext.roles`) — restrict-only.
    pub roles: Vec<String>,
    /// The sensitivity class of the data being sent.
    pub class: DataClass,
    /// The target provider.
    pub provider: ProviderKind,
}

/// Wraps an inner [`HttpTransport`], enforcing the [`EgressGate`] before any send.
///
/// On **allow**: the decision is audited and the call delegates to the inner
/// transport. On **deny**: the decision is audited and the call returns
/// [`ProviderError::Unsupported`] **without touching the inner transport** — so no
/// socket opens (default-deny). The P9.2 adapters use this in place of the bare
/// transport; the inner transport is the real `reqwest` client at deploy time.
pub struct GatedTransport<T: HttpTransport> {
    inner: T,
    gate: EgressGate,
    ctx: EgressContext,
    audit: Arc<dyn EgressAuditSink>,
}

impl<T: HttpTransport> GatedTransport<T> {
    /// Bind `inner` behind `gate` for the caller/data described by `ctx`, auditing
    /// to `audit`.
    pub fn new(
        inner: T,
        gate: EgressGate,
        ctx: EgressContext,
        audit: Arc<dyn EgressAuditSink>,
    ) -> Self {
        GatedTransport {
            inner,
            gate,
            ctx,
            audit,
        }
    }

    /// Run the gate for `request`, audit the outcome, and return `Ok(())` to
    /// proceed or `Err` to refuse (the inner transport is never called on `Err`).
    fn guard(&self, request: &HttpRequest) -> Result<(), ProviderError> {
        let host = host_of(&request.url);
        match self.gate.authorize(
            self.ctx.clearance,
            &self.ctx.roles,
            self.ctx.class,
            self.ctx.provider,
            &host,
        ) {
            Ok(_) => {
                self.audit.record(&EgressRecord {
                    allowed: true,
                    class: self.ctx.class,
                    provider: self.ctx.provider,
                    host,
                    denial: None,
                });
                Ok(())
            }
            Err(denial) => {
                let message = denial.to_string();
                self.audit.record(&EgressRecord {
                    allowed: false,
                    class: self.ctx.class,
                    provider: self.ctx.provider,
                    host,
                    denial: Some(message.clone()),
                });
                Err(ProviderError::Unsupported(message))
            }
        }
    }
}

impl<T: HttpTransport> HttpTransport for GatedTransport<T> {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, ProviderError> {
        self.guard(request)?;
        self.inner.send(request)
    }

    fn send_streaming(
        &self,
        request: &HttpRequest,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<u16, ProviderError> {
        self.guard(request)?;
        self.inner.send_streaming(request, on_line)
    }
}

/// Extract the bare host from a request URL (`https://api.openai.com:443/v1/x` →
/// `api.openai.com`): strip the scheme, take the authority up to the first
/// `/`/`?`/`#`, drop any `userinfo@` and `:port`.
fn host_of(url: &str) -> String {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    host.split(':').next().unwrap_or(host).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// An inner transport that counts how many times it was actually called — so a
    /// denied egress is proven to open no socket (`calls() == 0`).
    #[derive(Default)]
    struct CountingTransport {
        calls: AtomicUsize,
    }

    impl CountingTransport {
        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    impl HttpTransport for CountingTransport {
        fn send(&self, _request: &HttpRequest) -> Result<HttpResponse, ProviderError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(HttpResponse {
                status: 200,
                body: b"ok".to_vec(),
            })
        }

        fn send_streaming(
            &self,
            _request: &HttpRequest,
            _on_line: &mut dyn FnMut(&str),
        ) -> Result<u16, ProviderError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(200)
        }
    }

    fn roles(rs: &[&str]) -> Vec<String> {
        rs.iter().map(|s| s.to_string()).collect()
    }

    /// A gate that allows Public→Anthropic to api.anthropic.com only.
    fn anthropic_public_gate() -> EgressGate {
        EgressGate::new(
            RoleOverlay::new(),
            HostAllowlist::new().allow("api.anthropic.com"),
            DlpPolicy::new().allow(DataClass::Public, ProviderKind::Anthropic),
        )
    }

    fn request_to(host: &str) -> HttpRequest {
        HttpRequest {
            method: "POST",
            url: format!("https://{host}/v1/messages"),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    #[test]
    fn host_of_extracts_bare_host() {
        assert_eq!(host_of("https://api.openai.com/v1/chat"), "api.openai.com");
        assert_eq!(
            host_of("https://api.cerebras.ai:443/v1/x"),
            "api.cerebras.ai"
        );
        assert_eq!(host_of("http://user@host.example/p?q=1"), "host.example");
        assert_eq!(host_of("api.anthropic.com"), "api.anthropic.com");
    }

    #[test]
    fn allowed_egress_authorizes() {
        let gate = anthropic_public_gate();
        assert_eq!(
            gate.authorize(
                ClearanceLevel::L4,
                &[],
                DataClass::Public,
                ProviderKind::Anthropic,
                "api.anthropic.com",
            ),
            Ok(ClearanceLevel::L4)
        );
    }

    #[test]
    fn non_allowlisted_host_is_denied() {
        let gate = anthropic_public_gate();
        // Public→Anthropic is DLP-allowed, but the host is not allowlisted.
        let denied = gate.authorize(
            ClearanceLevel::L5,
            &[],
            DataClass::Public,
            ProviderKind::Anthropic,
            "evil.example.com",
        );
        assert!(matches!(
            denied,
            Err(EgressDenial::HostNotAllowlisted { .. })
        ));
    }

    #[test]
    fn host_allowlist_is_case_insensitive() {
        // A mixed-case request host must still match a lowercase allowlist entry
        // (DNS is case-insensitive); a gate must not refuse a legitimate host on case.
        let allowlist = HostAllowlist::new().allow("API.Anthropic.com");
        assert!(allowlist.is_allowed("api.anthropic.com"));
        assert!(allowlist.is_allowed("API.ANTHROPIC.COM"));
        assert!(!allowlist.is_allowed("api.openai.com"));
    }

    #[test]
    fn dlp_blocks_disallowed_pair_even_at_l5() {
        // Allowlist the OpenAI host, but DLP does NOT allow Public→OpenAI.
        let gate = EgressGate::new(
            RoleOverlay::new(),
            HostAllowlist::new().allow("api.openai.com"),
            DlpPolicy::new().allow(DataClass::Public, ProviderKind::Anthropic),
        );
        let denied = gate.authorize(
            ClearanceLevel::L5, // highest clearance — DLP is orthogonal
            &[],
            DataClass::Public,
            ProviderKind::OpenAI,
            "api.openai.com",
        );
        assert_eq!(
            denied,
            Err(EgressDenial::DlpBlocked {
                class: DataClass::Public,
                provider: ProviderKind::OpenAI,
            })
        );
    }

    #[test]
    fn restrict_only_role_blocks_high_class_then_passes_without_it() {
        // contractor caps clearance at L2; Restricted data needs L4.
        let gate = EgressGate::new(
            RoleOverlay::new().with_ceiling("contractor", ClearanceLevel::L2),
            HostAllowlist::new().allow("api.anthropic.com"),
            DlpPolicy::new().allow(DataClass::Restricted, ProviderKind::Anthropic),
        );
        // L4 caller, but the contractor role narrows them to L2 → below the L4 floor.
        assert_eq!(
            gate.authorize(
                ClearanceLevel::L4,
                &roles(&["contractor"]),
                DataClass::Restricted,
                ProviderKind::Anthropic,
                "api.anthropic.com",
            ),
            Err(EgressDenial::InsufficientClearance {
                required: ClearanceLevel::L4,
                effective: ClearanceLevel::L2,
                class: DataClass::Restricted,
            })
        );
        // The same caller without the restricting role passes (the role never elevates;
        // its absence simply doesn't restrict).
        assert!(gate
            .authorize(
                ClearanceLevel::L4,
                &[],
                DataClass::Restricted,
                ProviderKind::Anthropic,
                "api.anthropic.com",
            )
            .is_ok());
    }

    #[test]
    fn gated_transport_denies_before_socket_and_audits() {
        let audit = Arc::new(RecordingAudit::new());
        let gated = GatedTransport::new(
            CountingTransport::default(),
            anthropic_public_gate(),
            EgressContext {
                clearance: ClearanceLevel::L5,
                roles: Vec::new(),
                class: DataClass::Public,
                provider: ProviderKind::Anthropic,
            },
            audit.clone(),
        );
        // Allowlist has only api.anthropic.com; sending to the OpenAI host is denied.
        let result = gated.send(&request_to("api.openai.com"));
        assert!(matches!(result, Err(ProviderError::Unsupported(_))));
        // No socket opened.
        assert_eq!(gated.inner.calls(), 0);
        // The deny was audited.
        let records = audit.records();
        assert_eq!(records.len(), 1);
        assert!(!records[0].allowed);
        assert!(records[0].denial.is_some());
        assert_eq!(records[0].host, "api.openai.com");
    }

    #[test]
    fn gated_transport_allows_delegates_and_audits() {
        let audit = Arc::new(RecordingAudit::new());
        let gated = GatedTransport::new(
            CountingTransport::default(),
            anthropic_public_gate(),
            EgressContext {
                clearance: ClearanceLevel::L4,
                roles: Vec::new(),
                class: DataClass::Public,
                provider: ProviderKind::Anthropic,
            },
            audit.clone(),
        );
        let response = gated.send(&request_to("api.anthropic.com")).unwrap();
        assert_eq!(response.status, 200);
        // The inner transport was called exactly once.
        assert_eq!(gated.inner.calls(), 1);
        // The allow was audited.
        let records = audit.records();
        assert_eq!(records.len(), 1);
        assert!(records[0].allowed);
        assert!(records[0].denial.is_none());
    }

    #[test]
    fn empty_gate_is_default_deny() {
        let gate = EgressGate::default();
        // Nothing is allowlisted and DLP is empty → every egress is refused.
        assert!(gate
            .authorize(
                ClearanceLevel::L5,
                &[],
                DataClass::Public,
                ProviderKind::Anthropic,
                "api.anthropic.com",
            )
            .is_err());
    }
}
