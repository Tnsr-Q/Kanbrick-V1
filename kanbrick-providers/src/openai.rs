//! OpenAI / Cerebras wire adapter (P9.2, #102).
//!
//! One codec serves both: Cerebras speaks the OpenAI `POST /v1/chat/completions`
//! protocol on a distinct host. Unlike Anthropic, these providers report
//! **inclusive** token totals with nested subsets — `prompt_tokens` *includes*
//! `prompt_tokens_details.cached_tokens`, and `completion_tokens` *includes*
//! `completion_tokens_details.reasoning_tokens`. Mapping `input ← prompt_tokens`
//! directly would double-count the cached portion, so usage goes through
//! [`Usage::from_inclusive`], which subtracts each subset exactly once.
//!
//! Streaming usage only appears when `stream_options.include_usage` is set, in a
//! final chunk whose `choices` array is empty — this adapter requests it and maps
//! that one chunk (it is already the final inclusive total, so it is applied once,
//! never accumulated).
//!
//! [`Usage::from_inclusive`]: crate::Usage::from_inclusive

use serde::{Deserialize, Serialize};

use crate::wire::{self, HttpRequest, HttpTransport, SSE_DONE};
use crate::{
    ChatProvider, ChatRequest, ChatResponse, ProviderError, ProviderKind, Role, StopReason,
    TokenDelta, Usage,
};

/// Default OpenAI host.
const OPENAI_BASE_URL: &str = "https://api.openai.com";
/// Default Cerebras host (OpenAI-compatible protocol).
const CEREBRAS_BASE_URL: &str = "https://api.cerebras.ai";

/// A [`ChatProvider`] over the OpenAI Chat Completions protocol, generic over the
/// injected [`HttpTransport`]. The same type drives OpenAI and Cerebras; only the
/// base URL and reported [`ProviderKind`] differ.
pub struct OpenAiAdapter<T: HttpTransport> {
    transport: T,
    api_key: String,
    base_url: String,
    kind: ProviderKind,
}

impl<T: HttpTransport> OpenAiAdapter<T> {
    /// Build an adapter against the OpenAI host.
    pub fn openai(transport: T, api_key: impl Into<String>) -> Self {
        OpenAiAdapter {
            transport,
            api_key: api_key.into(),
            base_url: OPENAI_BASE_URL.to_string(),
            kind: ProviderKind::OpenAI,
        }
    }

    /// Build an adapter against the Cerebras host (same wire protocol).
    pub fn cerebras(transport: T, api_key: impl Into<String>) -> Self {
        OpenAiAdapter {
            transport,
            api_key: api_key.into(),
            base_url: CEREBRAS_BASE_URL.to_string(),
            kind: ProviderKind::Cerebras,
        }
    }

    /// Override the base URL (testing, proxies, the P9.6 egress front door).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn build_request(
        &self,
        request: &ChatRequest,
        stream: bool,
    ) -> Result<HttpRequest, ProviderError> {
        wire::check_temperature(request.temperature)?;

        // OpenAI carries the system prompt as the first message, not a top-level
        // field — prepend the explicit `system`, then pass System-role messages
        // through as `role: "system"`.
        let mut messages: Vec<WireMessage<'_>> = Vec::new();
        if let Some(system) = request.system.as_deref() {
            messages.push(WireMessage {
                role: "system",
                content: system,
            });
        }
        for message in &request.messages {
            messages.push(WireMessage {
                role: role_str(message.role),
                content: &message.content,
            });
        }

        let body = WireRequest {
            // NOTE: `max_tokens` is accepted by gpt-4o and Cerebras; the o-series
            // requires `max_completion_tokens` instead. The exact field is a
            // one-line switch confirmed when P9.6 captures live fixtures — until
            // egress exists there is nothing to validate it against.
            model: &request.model,
            messages,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            stream,
            stream_options: stream.then_some(StreamOptions {
                include_usage: true,
            }),
        };
        let body = serde_json::to_vec(&body)
            .map_err(|e| ProviderError::Decode(format!("encoding request: {e}")))?;

        Ok(HttpRequest {
            method: "POST",
            url: format!("{}/v1/chat/completions", self.base_url),
            headers: vec![
                (
                    "authorization".to_string(),
                    format!("Bearer {}", self.api_key),
                ),
                ("content-type".to_string(), "application/json".to_string()),
            ],
            body,
        })
    }
}

impl<T: HttpTransport> ChatProvider for OpenAiAdapter<T> {
    fn kind(&self) -> ProviderKind {
        self.kind
    }

    fn complete(&self, request: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        let http_request = self.build_request(request, false)?;
        let response = self.transport.send(&http_request)?;
        if !response.is_success() {
            return Err(wire::status_error(response.status, &response.body));
        }
        decode_response(&response.body)
    }

    fn stream(
        &self,
        request: &ChatRequest,
        on_delta: &mut dyn FnMut(TokenDelta),
    ) -> Result<crate::StreamOutcome, ProviderError> {
        let http_request = self.build_request(request, true)?;
        let mut acc = StreamAccumulator::default();
        let mut decode_error: Option<ProviderError> = None;

        let status = self.transport.send_streaming(&http_request, &mut |line| {
            if decode_error.is_some() {
                return;
            }
            if let Some(payload) = wire::sse_data(line) {
                if payload == SSE_DONE {
                    return;
                }
                if let Err(e) = acc.consume(payload, on_delta) {
                    decode_error = Some(e);
                }
            }
        })?;

        if !(200..300).contains(&status) {
            return Err(ProviderError::Api {
                status,
                message: "streaming request failed".to_string(),
            });
        }
        if let Some(e) = decode_error {
            return Err(e);
        }
        Ok(acc.into_outcome())
    }
}

/// Normalized role → OpenAI wire role.
fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

// --- wire types: request ----------------------------------------------------

#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    messages: Vec<WireMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'a str,
    content: &'a str,
}

// --- wire types: response ---------------------------------------------------

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    model: String,
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    message: Option<WireContent>,
    #[serde(default)]
    delta: Option<WireContent>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireContent {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: TokenDetails,
    #[serde(default)]
    completion_tokens_details: TokenDetails,
}

#[derive(Deserialize, Default)]
struct TokenDetails {
    #[serde(default)]
    cached_tokens: u64,
    #[serde(default)]
    reasoning_tokens: u64,
}

impl WireUsage {
    /// Inclusive totals with nested subsets → disjoint [`Usage`]. OpenAI bills no
    /// separate cache *write*, so `cache_write` is `0`.
    fn normalize(&self) -> Usage {
        Usage::from_inclusive(
            self.prompt_tokens,
            self.prompt_tokens_details.cached_tokens,
            0,
            self.completion_tokens,
            self.completion_tokens_details.reasoning_tokens,
        )
    }
}

/// Map OpenAI's `finish_reason` onto the normalized [`StopReason`].
fn map_finish_reason(raw: Option<&str>) -> StopReason {
    match raw {
        Some("stop") => StopReason::EndTurn,
        Some("length") => StopReason::MaxTokens,
        Some(other) => StopReason::Other(other.to_string()),
        None => StopReason::EndTurn,
    }
}

fn decode_response(body: &[u8]) -> Result<ChatResponse, ProviderError> {
    let wire: WireResponse = serde_json::from_slice(body)
        .map_err(|e| ProviderError::Decode(format!("decoding response: {e}")))?;
    let first = wire.choices.first();
    let content = first
        .and_then(|c| c.message.as_ref())
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    let stop_reason = map_finish_reason(first.and_then(|c| c.finish_reason.as_deref()));
    let usage = wire.usage.unwrap_or_default().normalize();
    Ok(ChatResponse {
        model: wire.model,
        content,
        usage,
        stop_reason,
    })
}

// --- streaming accumulator --------------------------------------------------

/// Folds OpenAI SSE chunks into a [`crate::StreamOutcome`]. Each chunk's
/// `choices[0].delta.content` is forwarded as a [`TokenDelta`]; `finish_reason`
/// sets the stop reason; the terminal usage-only chunk (empty `choices`, present
/// `usage`) sets the final disjoint [`Usage`] once.
#[derive(Default)]
struct StreamAccumulator {
    usage: Usage,
    stop_reason: Option<StopReason>,
}

impl StreamAccumulator {
    fn consume(
        &mut self,
        payload: &str,
        on_delta: &mut dyn FnMut(TokenDelta),
    ) -> Result<(), ProviderError> {
        let chunk: WireResponse = serde_json::from_str(payload)
            .map_err(|e| ProviderError::Decode(format!("decoding stream chunk: {e}")))?;
        if let Some(choice) = chunk.choices.first() {
            if let Some(text) = choice.delta.as_ref().and_then(|d| d.content.clone()) {
                if !text.is_empty() {
                    on_delta(TokenDelta { text });
                }
            }
            if let Some(reason) = choice.finish_reason.as_deref() {
                self.stop_reason = Some(map_finish_reason(Some(reason)));
            }
        }
        if let Some(usage) = chunk.usage {
            self.usage = usage.normalize();
        }
        Ok(())
    }

    fn into_outcome(self) -> crate::StreamOutcome {
        crate::StreamOutcome {
            usage: self.usage,
            stop_reason: self.stop_reason.unwrap_or(StopReason::EndTurn),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::RecordedTransport;

    const COMPLETION_FIXTURE: &str = r#"{
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "model": "gpt-4o-2024-08-06",
        "choices": [
            {"index": 0, "message": {"role": "assistant", "content": "Hello there"},
             "finish_reason": "stop"}
        ],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
            "prompt_tokens_details": {"cached_tokens": 30},
            "completion_tokens_details": {"reasoning_tokens": 10}
        }
    }"#;

    #[test]
    fn complete_decodes_content_and_inclusive_usage_without_double_count() {
        let adapter = OpenAiAdapter::openai(
            RecordedTransport::responding(200, COMPLETION_FIXTURE),
            "sk-test",
        );
        let resp = adapter
            .complete(&ChatRequest::new("gpt-4o").user("hi"))
            .unwrap();
        assert_eq!(resp.content, "Hello there");
        assert_eq!(resp.model, "gpt-4o-2024-08-06");
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
        // prompt_tokens(100) includes cached(30); completion(50) includes
        // reasoning(10). Disjoint buckets must subtract the subsets exactly once.
        assert_eq!(resp.usage.input, 70); // 100 - 30
        assert_eq!(resp.usage.cache_read, 30);
        assert_eq!(resp.usage.cache_write, 0); // OpenAI has no separate write
        assert_eq!(resp.usage.output, 40); // 50 - 10
        assert_eq!(resp.usage.reasoning, 10);
        // The disjoint total reconstructs the wire's total_tokens exactly.
        assert_eq!(resp.usage.total(), 150);
        assert_eq!(resp.usage.total_input(), 100);
        assert_eq!(resp.usage.total_output(), 50);
    }

    #[test]
    fn build_request_prepends_system_message_and_sets_bearer() {
        let adapter = OpenAiAdapter::openai(
            RecordedTransport::responding(200, COMPLETION_FIXTURE),
            "sk-secret",
        );
        let req = ChatRequest::new("gpt-4o")
            .with_system("be terse")
            .with_max_tokens(128)
            .user("hello");
        let _ = adapter.complete(&req).unwrap();

        let sent = adapter.transport.last_request();
        assert_eq!(sent.url, "https://api.openai.com/v1/chat/completions");
        assert!(sent
            .headers
            .iter()
            .any(|(k, v)| k == "authorization" && v == "Bearer sk-secret"));

        let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
        assert_eq!(body["max_tokens"], 128);
        // System is the first message, not a top-level field.
        assert!(body.get("system").is_none());
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "be terse");
        assert_eq!(body["messages"][1]["role"], "user");
        // Non-streaming requests carry no stream_options.
        assert_eq!(body["stream"], false);
        assert!(body.get("stream_options").is_none());
    }

    #[test]
    fn cerebras_uses_its_own_host_and_kind() {
        let adapter =
            OpenAiAdapter::cerebras(RecordedTransport::responding(200, COMPLETION_FIXTURE), "k");
        assert_eq!(adapter.kind(), ProviderKind::Cerebras);
        let _ = adapter
            .complete(&ChatRequest::new("llama-3.3-70b").user("hi"))
            .unwrap();
        assert_eq!(
            adapter.transport.last_request().url,
            "https://api.cerebras.ai/v1/chat/completions"
        );
    }

    #[test]
    fn complete_maps_status_codes_to_errors() {
        let limited = OpenAiAdapter::openai(
            RecordedTransport::responding(429, r#"{"error":{"message":"slow down"}}"#),
            "k",
        );
        assert!(matches!(
            limited.complete(&ChatRequest::new("m").user("x")),
            Err(ProviderError::RateLimited)
        ));
    }

    #[test]
    fn stream_requests_usage_emits_deltas_and_maps_final_usage() {
        let lines = vec![
            r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{"content":"Hel"},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{"content":"lo"},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            r#"data: {"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":50,"prompt_tokens_details":{"cached_tokens":30},"completion_tokens_details":{"reasoning_tokens":10}}}"#,
            r#"data: [DONE]"#,
        ];
        let adapter = OpenAiAdapter::openai(RecordedTransport::streaming(lines, 200), "k");

        // The request body must opt into streamed usage.
        let mut text = String::new();
        let outcome = adapter
            .stream(&ChatRequest::new("gpt-4o").user("hi"), &mut |d| {
                text.push_str(&d.text)
            })
            .unwrap();
        assert_eq!(text, "Hello");
        assert_eq!(outcome.stop_reason, StopReason::EndTurn);
        assert_eq!(outcome.usage.input, 70);
        assert_eq!(outcome.usage.output, 40);
        assert_eq!(outcome.usage.reasoning, 10);
        assert_eq!(outcome.usage.total(), 150);

        let body: serde_json::Value =
            serde_json::from_slice(&adapter.transport.last_request().body).unwrap();
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }
}
