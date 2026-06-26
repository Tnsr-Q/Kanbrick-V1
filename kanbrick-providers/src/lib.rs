//! BYO-AI provider abstraction (L5 Cockpit, Phase 9 — #80).
//!
//! One host-side trait, [`ChatProvider`], over the cloud LLM providers
//! (Claude / OpenAI / Cerebras), so the rest of the system — loops, messenger,
//! the token ledger — talks to a single interface instead of three SDKs. This
//! crate (P9.1, #101) defines the **trait and the normalized types only**; the
//! wire adapters land in P9.2 (#102), key custody in P9.3 (#103), streaming UI in
//! P9.4 (#104), ledger capture in P9.5 (#105), and the DLP + egress send-gate in
//! P9.6 (#106). Per ADR-0017 this crate is the *only* place core data may egress,
//! and only behind the P9.6 gate — there is no network in P9.1.
//!
//! ## Design
//!
//! - **Object-safe.** [`ChatProvider`] uses only `&self`, borrowed arguments, and
//!   a `&mut dyn FnMut` sink, so providers are stored as `Box<dyn ChatProvider>`
//!   / `Arc<dyn ChatProvider>` and selected at runtime (the P9.4 selector).
//! - **Runtime-agnostic streaming.** [`ChatProvider::stream`] pushes
//!   [`TokenDelta`]s to a caller-supplied sink rather than returning a `Stream`,
//!   so the crate pulls no async runtime. The P9.4 Tauri-`Channel` adapter passes
//!   a sink that forwards each delta to the webview; a blocking caller passes a
//!   closure that appends to a buffer. It returns a [`StreamOutcome`] so callers
//!   get the final [`Usage`] *and* [`StopReason`] without a second call.
//! - **Normalized, disjoint [`Usage`].** Providers report token usage with
//!   *different* shapes (Anthropic excludes cache tokens from its input count;
//!   OpenAI nests cached/reasoning tokens). [`Usage`]'s fields are **mutually
//!   disjoint**, so a total is a plain sum and the P9.2 adapters cannot
//!   double-count — that invariant is the contract this crate exists to pin (see
//!   [`Usage::from_inclusive`] for the OpenAI mapping that would otherwise
//!   double-count).
//!
//! ## Modules
//!
//! - [`wire`] — the HTTP transport seam (`HttpTransport`) the P9.2 adapters call,
//!   plus shared SSE / status-mapping helpers. No live transport ships here: the
//!   real `reqwest` client lands in P9.6 behind the ADR-0017 egress gate.
//! - [`anthropic`] — the Claude Messages-API adapter (disjoint usage).
//! - [`openai`] — the OpenAI / Cerebras Chat-Completions adapter (inclusive usage
//!   → [`Usage::from_inclusive`]).

pub mod anthropic;
pub mod openai;
pub mod wire;

#[cfg(test)]
mod test_support;

use serde::{Deserialize, Serialize};

/// A cloud LLM provider, normalized. The wire details (`base_url`, auth, model
/// ids) live in the P9.2 adapters; this is the stable identifier the rest of the
/// system keys on — the DLP/egress gate (ADR-0010/0017, P9.6 keys its
/// `(data-class → provider)` policy on this enum, retiring the throwaway
/// `probes/rbac-overlay::Provider`), the selector UI, and the token ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// Anthropic (Claude).
    Anthropic,
    /// OpenAI (GPT / o-series).
    OpenAI,
    /// Cerebras — OpenAI-compatible wire protocol, distinct host.
    Cerebras,
}

impl ProviderKind {
    /// Lower-case stable token (matches the serde representation).
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::OpenAI => "openai",
            ProviderKind::Cerebras => "cerebras",
        }
    }

    /// The canonical/default API host for this provider.
    ///
    /// ADR-0017's egress allowlist is *per-tenant, default-deny* graph data — this
    /// constant is not that allowlist, it is the natural default entry and the
    /// host the P9.6 egress check matches on. The check keys on the host, so
    /// Cerebras (OpenAI-compatible protocol, distinct host) is allowlisted under
    /// its own host rather than OpenAI's.
    pub fn canonical_host(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "api.anthropic.com",
            ProviderKind::OpenAI => "api.openai.com",
            ProviderKind::Cerebras => "api.cerebras.ai",
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The author of a chat message, normalized across providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// System / developer instructions.
    System,
    /// The end user.
    User,
    /// The model.
    Assistant,
}

/// One message in a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Who authored the message.
    pub role: Role,
    /// The message text.
    pub content: String,
}

impl Message {
    /// Build a message.
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Message {
            role,
            content: content.into(),
        }
    }
}

/// A provider-agnostic completion request. The P9.2 adapters translate this into
/// each provider's wire format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Provider-specific model id (e.g. `claude-opus-4-8`, `gpt-4o`).
    pub model: String,
    /// The conversation, in order.
    pub messages: Vec<Message>,
    /// Optional top-level system prompt (providers that take one hoist it here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Optional output-token ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Optional sampling temperature. P9.2 should validate the range and reject
    /// `NaN` before sending — several providers 400 on out-of-range values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

impl ChatRequest {
    /// Start a request for `model` with an empty conversation.
    pub fn new(model: impl Into<String>) -> Self {
        ChatRequest {
            model: model.into(),
            messages: Vec::new(),
            system: None,
            max_tokens: None,
            temperature: None,
        }
    }

    /// Set the top-level system prompt (builder).
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Cap the output tokens (builder).
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Set the sampling temperature (builder).
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Append a message (builder).
    pub fn push(mut self, role: Role, content: impl Into<String>) -> Self {
        self.messages.push(Message::new(role, content));
        self
    }

    /// Append a user message (builder).
    pub fn user(self, content: impl Into<String>) -> Self {
        self.push(Role::User, content)
    }
}

/// Why generation stopped, normalized across providers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished its turn.
    EndTurn,
    /// The output-token ceiling was hit.
    MaxTokens,
    /// A stop sequence was emitted.
    StopSequence,
    /// Anything a provider reports that doesn't map to the above.
    Other(String),
}

/// Normalized token usage with **mutually disjoint** fields.
///
/// Every field counts a distinct bucket of tokens, so a total is a plain sum and
/// the P9.2 wire adapters — which receive differently-shaped raw usage from each
/// provider — cannot double-count. This disjointness is the contract:
///
/// - `input` excludes anything in `cache_read` / `cache_write`.
/// - `output` excludes `reasoning`.
///
/// **P9.2 mapping (the double-count hazard lives here):**
///
/// - **Anthropic** already reports disjoint counts — `input_tokens` excludes
///   cache, with separate `cache_read_input_tokens` /
///   `cache_creation_input_tokens`. Map straight onto the fields with a struct
///   literal.
/// - **OpenAI / Cerebras** report *inclusive* totals with nested subsets:
///   `prompt_tokens` includes `prompt_tokens_details.cached_tokens`, and
///   `completion_tokens` includes `completion_tokens_details.reasoning_tokens`.
///   Mapping `input ← prompt_tokens` directly double-counts the cached portion —
///   use [`Usage::from_inclusive`], which subtracts the subsets exactly once.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Uncached prompt (input) tokens — **not** including cache read/write.
    #[serde(default)]
    pub input: u64,
    /// Visible completion (output) tokens — **not** including `reasoning`.
    #[serde(default)]
    pub output: u64,
    /// Prompt tokens served from the provider's prompt cache (discounted).
    #[serde(default)]
    pub cache_read: u64,
    /// Prompt tokens written into the provider's prompt cache (one-time cost).
    #[serde(default)]
    pub cache_write: u64,
    /// Hidden reasoning / thinking tokens (o-series, extended thinking), billed
    /// as output but tracked separately.
    #[serde(default)]
    pub reasoning: u64,
}

impl Usage {
    /// Build a `Usage` from a provider that reports **inclusive** totals with
    /// nested subsets (OpenAI / Cerebras: `prompt_tokens` includes
    /// `cached_tokens`; `completion_tokens` includes `reasoning_tokens`). The
    /// subsets are subtracted so the result is disjoint and cannot double-count.
    /// Saturating, so malformed input (a subset larger than its total) clamps to
    /// `0` rather than wrapping. OpenAI bills no separate cache *write*, so pass
    /// `cache_write = 0` there.
    ///
    /// Anthropic already reports disjoint counts — build it with a struct literal,
    /// not this constructor.
    pub fn from_inclusive(
        prompt_tokens: u64,
        cached_tokens: u64,
        cache_write: u64,
        completion_tokens: u64,
        reasoning_tokens: u64,
    ) -> Self {
        Usage {
            input: prompt_tokens.saturating_sub(cached_tokens),
            output: completion_tokens.saturating_sub(reasoning_tokens),
            cache_read: cached_tokens,
            cache_write,
            reasoning: reasoning_tokens,
        }
    }

    /// Total prompt-side tokens (`input + cache_read + cache_write`).
    pub fn total_input(&self) -> u64 {
        self.input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }

    /// Total completion-side tokens (`output + reasoning`).
    pub fn total_output(&self) -> u64 {
        self.output.saturating_add(self.reasoning)
    }

    /// Grand total across every disjoint bucket.
    pub fn total(&self) -> u64 {
        self.total_input().saturating_add(self.total_output())
    }

    /// Accumulate another usage into this one — used to fold per-chunk usage
    /// while streaming. Saturating, so a runaway count can never wrap.
    pub fn accumulate(&mut self, other: &Usage) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.reasoning = self.reasoning.saturating_add(other.reasoning);
    }
}

/// A response from a single completion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    /// The model that produced the response.
    pub model: String,
    /// The full assistant text.
    pub content: String,
    /// Normalized token usage.
    pub usage: Usage,
    /// Why generation stopped.
    pub stop_reason: StopReason,
}

/// One streamed chunk of assistant text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenDelta {
    /// The incremental text for this chunk.
    pub text: String,
}

/// The terminal result of a stream: the accumulated [`Usage`] plus why it
/// stopped. Returned by [`ChatProvider::stream`] so a caller (the P9.4 UI, the
/// P9.5 ledger) does not need a second call to learn the stop reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamOutcome {
    /// Total token usage over the whole stream.
    pub usage: Usage,
    /// Why generation stopped.
    pub stop_reason: StopReason,
}

/// A failure talking to a provider, normalized across wire protocols. (P9.1 has
/// no network; the type is defined here because it is part of the trait
/// contract the P9.2 adapters implement against.)
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// Transport / connection failure (DNS, TLS, socket).
    #[error("provider transport error: {0}")]
    Transport(String),
    /// The provider rejected the credentials.
    #[error("provider authentication failed")]
    Auth,
    /// The provider applied rate limiting.
    #[error("provider rate limited")]
    RateLimited,
    /// The provider returned a non-success status with a message.
    #[error("provider error (status {status}): {message}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Provider-supplied message.
        message: String,
    },
    /// The provider response could not be decoded into the normalized types.
    #[error("failed to decode provider response: {0}")]
    Decode(String),
    /// The requested capability is not supported by this provider.
    #[error("unsupported by this provider: {0}")]
    Unsupported(String),
}

/// The single host-side interface over a cloud LLM provider.
///
/// Object-safe (`dyn ChatProvider`) and runtime-agnostic. Implemented per wire
/// protocol in P9.2; selected at runtime in P9.4. Egress through any
/// implementation is gated by P9.6 (ADR-0010 DLP + ADR-0017 allowlist).
pub trait ChatProvider: Send + Sync {
    /// Which provider this is (for selection, audit, DLP, and the token ledger).
    fn kind(&self) -> ProviderKind;

    /// Run a request to completion and return the full response + usage.
    fn complete(&self, request: &ChatRequest) -> Result<ChatResponse, ProviderError>;

    /// Stream a request, pushing each [`TokenDelta`] to `on_delta`, and return the
    /// terminal [`StreamOutcome`] (accumulated [`Usage`] + [`StopReason`]).
    ///
    /// The default implementation falls back to [`complete`](ChatProvider::complete)
    /// and emits the whole response as a single delta, so a provider that has not
    /// implemented native streaming still satisfies the streaming surface.
    fn stream(
        &self,
        request: &ChatRequest,
        on_delta: &mut dyn FnMut(TokenDelta),
    ) -> Result<StreamOutcome, ProviderError> {
        let response = self.complete(request)?;
        on_delta(TokenDelta {
            text: response.content,
        });
        Ok(StreamOutcome {
            usage: response.usage,
            stop_reason: response.stop_reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A deterministic in-test provider — stands in for the P9.2 wire adapters so
    /// the trait + types are exercised with no network.
    #[derive(Default)]
    struct StubProvider {
        reply: String,
        usage: Usage,
    }

    impl ChatProvider for StubProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Anthropic
        }
        fn complete(&self, request: &ChatRequest) -> Result<ChatResponse, ProviderError> {
            Ok(ChatResponse {
                model: request.model.clone(),
                content: self.reply.clone(),
                usage: self.usage,
                stop_reason: StopReason::EndTurn,
            })
        }
    }

    #[test]
    fn request_builder_constructs_conversation() {
        let req = ChatRequest::new("claude-opus-4-8")
            .with_system("be terse")
            .with_max_tokens(256)
            .with_temperature(0.2)
            .user("hello");
        assert_eq!(req.model, "claude-opus-4-8");
        assert_eq!(req.system.as_deref(), Some("be terse"));
        assert_eq!(req.max_tokens, Some(256));
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(req.messages[0].content, "hello");
    }

    #[test]
    fn usage_buckets_are_disjoint_and_total_is_a_sum() {
        let u = Usage {
            input: 100,
            output: 40,
            cache_read: 10,
            cache_write: 5,
            reasoning: 7,
        };
        assert_eq!(u.total_input(), 115); // 100 + 10 + 5
        assert_eq!(u.total_output(), 47); // 40 + 7
        assert_eq!(u.total(), 162); // every disjoint bucket, summed once
    }

    #[test]
    fn usage_from_inclusive_subtracts_nested_subsets() {
        // OpenAI-style: prompt_tokens (100) includes cached (30); completion (50)
        // includes reasoning (10). Disjoint result must not double-count.
        let u = Usage::from_inclusive(100, 30, 0, 50, 10);
        assert_eq!(u.input, 70); // 100 - 30
        assert_eq!(u.cache_read, 30);
        assert_eq!(u.cache_write, 0); // OpenAI has no separate write
        assert_eq!(u.output, 40); // 50 - 10
        assert_eq!(u.reasoning, 10);
        // The disjoint totals reconstruct the provider's inclusive totals exactly.
        assert_eq!(u.total_input(), 100);
        assert_eq!(u.total_output(), 50);
        assert_eq!(u.total(), 150);

        // Malformed (subset > total) saturates to 0 rather than wrapping.
        let bad = Usage::from_inclusive(5, 10, 0, 3, 9);
        assert_eq!(bad.input, 0);
        assert_eq!(bad.output, 0);
    }

    #[test]
    fn usage_serde_round_trips_and_defaults_missing_fields() {
        let u = Usage {
            input: 12,
            output: 3,
            cache_read: 1,
            cache_write: 0,
            reasoning: 0,
        };
        let json = serde_json::to_string(&u).unwrap();
        let back: Usage = serde_json::from_str(&json).unwrap();
        assert_eq!(u, back);

        // A provider that omits the cache/reasoning fields yields zeros, not an
        // error — the P9.2 adapters rely on this.
        let partial: Usage = serde_json::from_str(r#"{"input": 9, "output": 4}"#).unwrap();
        assert_eq!(partial.input, 9);
        assert_eq!(partial.output, 4);
        assert_eq!(partial.cache_read, 0);
        assert_eq!(partial.reasoning, 0);
    }

    #[test]
    fn usage_accumulates_while_streaming() {
        let mut total = Usage::default();
        total.accumulate(&Usage {
            input: 50,
            output: 1,
            ..Usage::default()
        });
        total.accumulate(&Usage {
            output: 9,
            reasoning: 4,
            ..Usage::default()
        });
        assert_eq!(total.input, 50);
        assert_eq!(total.output, 10);
        assert_eq!(total.reasoning, 4);
        assert_eq!(total.total(), 64);
        // The disjointness invariant survives folding.
        assert_eq!(total.total(), total.total_input() + total.total_output());
    }

    #[test]
    fn usage_accumulate_saturates_near_max() {
        let mut total = Usage {
            input: u64::MAX,
            ..Usage::default()
        };
        total.accumulate(&Usage {
            input: 10,
            ..Usage::default()
        });
        assert_eq!(total.input, u64::MAX); // clamped, never wrapped
        assert_eq!(total.total(), u64::MAX);
    }

    #[test]
    fn complete_returns_normalized_response() {
        let provider = StubProvider {
            reply: "hi there".into(),
            usage: Usage {
                input: 5,
                output: 2,
                ..Usage::default()
            },
        };
        let resp = provider
            .complete(&ChatRequest::new("claude-opus-4-8").user("hi"))
            .unwrap();
        assert_eq!(resp.content, "hi there");
        assert_eq!(resp.usage.output, 2);
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn default_stream_emits_full_content_and_returns_outcome() {
        let provider = StubProvider {
            reply: "streamed".into(),
            usage: Usage {
                input: 3,
                output: 2,
                ..Usage::default()
            },
        };
        let mut chunks: Vec<String> = Vec::new();
        let outcome = provider
            .stream(&ChatRequest::new("m").user("go"), &mut |d| {
                chunks.push(d.text)
            })
            .unwrap();
        assert_eq!(chunks, vec!["streamed".to_string()]);
        assert_eq!(outcome.usage.total(), 5);
        assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn chat_provider_is_object_safe_via_box_and_arc() {
        // Compile-time proof the trait is usable as `dyn` behind both pointers
        // (the P9.4 selector holds `Box`/`Arc<dyn ChatProvider>`).
        let boxed: Box<dyn ChatProvider> = Box::new(StubProvider {
            reply: "ok".into(),
            usage: Usage::default(),
        });
        assert_eq!(boxed.kind(), ProviderKind::Anthropic);
        let resp = boxed.complete(&ChatRequest::new("m").user("x")).unwrap();
        assert_eq!(resp.content, "ok");

        let shared: Arc<dyn ChatProvider> = Arc::new(StubProvider::default());
        assert_eq!(shared.kind(), ProviderKind::Anthropic);
    }

    #[test]
    fn provider_kind_host_and_display() {
        assert_eq!(
            ProviderKind::Anthropic.canonical_host(),
            "api.anthropic.com"
        );
        assert_eq!(ProviderKind::OpenAI.canonical_host(), "api.openai.com");
        assert_eq!(ProviderKind::Cerebras.canonical_host(), "api.cerebras.ai");
        assert_eq!(ProviderKind::OpenAI.to_string(), "openai");
    }
}
