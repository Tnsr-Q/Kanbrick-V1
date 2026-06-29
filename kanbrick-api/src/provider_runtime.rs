//! Provider-step runtime seam for the loop run engine (P11.4, ADR-0019).
//!
//! A loop *provider step* (P11.4) runs an LLM completion instead of a WASM guest.
//! The security-load-bearing rule (ADR-0002/0009): **the step picks the model only**
//! — it never carries a credential — and **the host injects the key**, resolved from
//! custody by the run's *caller* identity. This module is the seam where the resolved
//! key meets a provider implementation:
//!
//! * [`ProviderFactory`] builds a [`ChatProvider`] from a provider kind + the
//!   host-resolved key. The run engine resolves the key and calls `build`; a step
//!   can never supply it.
//! * The default [`EchoProviderFactory`] builds a no-network [`EchoProvider`] (it
//!   echoes the prompt back) — the slice ships **no live egress**, matching the
//!   ADR-0017 / P9.4 / P9.6 discipline (no `reqwest` in core/CI). At deploy the real
//!   factory composes the `kanbrick-providers` wire adapters behind the
//!   `kanbrick-egress` `GatedTransport` (per-tenant allowlist + DLP) over the same
//!   `ChatProvider` interface, injected via
//!   [`AppState::with_provider_factory`](crate::AppState::with_provider_factory).

use kanbrick_providers::{
    ChatProvider, ChatRequest, ChatResponse, ProviderError, ProviderKind, Role, StopReason, Usage,
};

/// Builds a [`ChatProvider`] for a provider step from a provider kind and the
/// host-resolved API key. The key is resolved by the run engine from custody **by the
/// caller's identity** and injected here — a loop step never carries it.
pub trait ProviderFactory: Send + Sync {
    /// Build a provider for `kind` authenticated by `api_key`.
    fn build(&self, kind: ProviderKind, api_key: &str) -> Box<dyn ChatProvider>;
}

/// The default, no-network factory: builds an [`EchoProvider`]. Ships in place of a
/// live transport (2A) so provider steps are exercised end to end with zero egress;
/// the real adapter+egress-gate factory is injected at deploy.
#[derive(Debug, Default, Clone, Copy)]
pub struct EchoProviderFactory;

impl ProviderFactory for EchoProviderFactory {
    fn build(&self, kind: ProviderKind, _api_key: &str) -> Box<dyn ChatProvider> {
        // The key is intentionally unused by the echo stub; a real adapter would
        // authenticate with it. Resolving + injecting it host-side is the point.
        Box::new(EchoProvider { kind })
    }
}

/// A no-network [`ChatProvider`] that returns the last user message as its content.
/// Stands in for the P9.2 wire adapters until a real factory is injected at deploy.
struct EchoProvider {
    kind: ProviderKind,
}

impl ChatProvider for EchoProvider {
    fn kind(&self) -> ProviderKind {
        self.kind
    }

    fn complete(&self, request: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        let content = request
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, Role::User))
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let tokens = content.split_whitespace().count() as u64;
        Ok(ChatResponse {
            model: request.model.clone(),
            content,
            usage: Usage {
                input: tokens,
                output: tokens,
                ..Usage::default()
            },
            stop_reason: StopReason::EndTurn,
        })
    }
}

/// Parse a stored provider string (the serde-lowercase token) into a [`ProviderKind`].
/// Returns `None` for an unknown token, so the run engine / create route can reject it.
pub(crate) fn parse_provider(token: &str) -> Option<ProviderKind> {
    match token {
        "anthropic" => Some(ProviderKind::Anthropic),
        "openai" => Some(ProviderKind::OpenAI),
        "cerebras" => Some(ProviderKind::Cerebras),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_provider_maps_known_tokens_and_rejects_unknown() {
        assert_eq!(parse_provider("anthropic"), Some(ProviderKind::Anthropic));
        assert_eq!(parse_provider("openai"), Some(ProviderKind::OpenAI));
        assert_eq!(parse_provider("cerebras"), Some(ProviderKind::Cerebras));
        assert_eq!(parse_provider("gemini"), None);
        assert_eq!(parse_provider(""), None);
    }

    #[test]
    fn echo_factory_builds_a_provider_that_returns_the_prompt() {
        let provider = EchoProviderFactory.build(ProviderKind::Anthropic, "sk-ignored");
        assert_eq!(provider.kind(), ProviderKind::Anthropic);
        let request = ChatRequest::new("claude-opus-4-8").user("summarize this");
        let response = provider.complete(&request).unwrap();
        assert_eq!(response.content, "summarize this");
        assert_eq!(response.usage.output, 2);
    }
}
