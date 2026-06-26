//! Anthropic (Claude) wire adapter (P9.2, #102).
//!
//! Talks the `POST /v1/messages` Messages API. Anthropic already reports
//! **disjoint** token counts — `input_tokens` excludes cache, with separate
//! `cache_read_input_tokens` / `cache_creation_input_tokens` — so usage maps onto
//! [`Usage`] with a struct literal (no [`Usage::from_inclusive`]). The streaming
//! path is where care is required: `output_tokens` arrives **cumulative** in the
//! terminal `message_delta`, so it is *overwritten*, never accumulated.
//!
//! [`Usage::from_inclusive`]: crate::Usage::from_inclusive

use serde::{Deserialize, Serialize};

use crate::wire::{self, HttpRequest, HttpTransport};
use crate::{
    ChatProvider, ChatRequest, ChatResponse, ProviderError, ProviderKind, Role, StopReason,
    TokenDelta, Usage,
};

/// Default `https://api.anthropic.com` host.
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
/// The Messages API requires an explicit `max_tokens`; used when the request
/// leaves it unset.
const DEFAULT_MAX_TOKENS: u32 = 4096;
/// Pinned Messages API version (sent as the `anthropic-version` header).
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// An [`ChatProvider`] over Anthropic's Messages API, generic over the injected
/// [`HttpTransport`]. The key is held in memory for P9.2; P9.3 sources it from the
/// Stronghold enclave instead.
pub struct AnthropicAdapter<T: HttpTransport> {
    transport: T,
    api_key: String,
    base_url: String,
    default_max_tokens: u32,
}

impl<T: HttpTransport> AnthropicAdapter<T> {
    /// Build an adapter against the default host.
    pub fn new(transport: T, api_key: impl Into<String>) -> Self {
        AnthropicAdapter {
            transport,
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            default_max_tokens: DEFAULT_MAX_TOKENS,
        }
    }

    /// Override the base URL (testing, proxies, the P9.6 egress front door).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Override the `max_tokens` applied when a request leaves it unset.
    pub fn with_default_max_tokens(mut self, max_tokens: u32) -> Self {
        self.default_max_tokens = max_tokens;
        self
    }

    /// Build the `HttpRequest` for a chat request. `stream` toggles SSE.
    fn build_request(
        &self,
        request: &ChatRequest,
        stream: bool,
    ) -> Result<HttpRequest, ProviderError> {
        wire::check_temperature(request.temperature)?;

        // Anthropic takes the system prompt at the top level, never as a message
        // in the array — hoist both the explicit `system` field and any
        // System-role messages into one block.
        let mut system_parts: Vec<&str> = Vec::new();
        if let Some(system) = request.system.as_deref() {
            system_parts.push(system);
        }
        let mut messages: Vec<WireMessage<'_>> = Vec::new();
        for message in &request.messages {
            match message.role {
                Role::System => system_parts.push(&message.content),
                Role::User => messages.push(WireMessage {
                    role: "user",
                    content: &message.content,
                }),
                Role::Assistant => messages.push(WireMessage {
                    role: "assistant",
                    content: &message.content,
                }),
            }
        }
        let system = if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        };

        let body = WireRequest {
            model: &request.model,
            max_tokens: request.max_tokens.unwrap_or(self.default_max_tokens),
            system,
            messages,
            temperature: request.temperature,
            stream,
        };
        let body = serde_json::to_vec(&body)
            .map_err(|e| ProviderError::Decode(format!("encoding request: {e}")))?;

        Ok(HttpRequest {
            method: "POST",
            url: format!("{}/v1/messages", self.base_url),
            headers: vec![
                ("x-api-key".to_string(), self.api_key.clone()),
                (
                    "anthropic-version".to_string(),
                    ANTHROPIC_VERSION.to_string(),
                ),
                ("content-type".to_string(), "application/json".to_string()),
            ],
            body,
        })
    }
}

impl<T: HttpTransport> ChatProvider for AnthropicAdapter<T> {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Anthropic
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
                if let Err(e) = acc.consume(payload, on_delta) {
                    decode_error = Some(e);
                }
            }
        })?;

        // A streaming endpoint can still answer non-2xx (e.g. 401 before the
        // event stream opens); the body in that case is a normal JSON error,
        // surfaced verbatim.
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

// --- wire types: request (Serialize) ---------------------------------------

#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<WireMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    stream: bool,
}

#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'a str,
    content: &'a str,
}

// --- wire types: response (Deserialize) ------------------------------------

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    model: String,
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: WireUsage,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

impl WireUsage {
    /// Anthropic counts are already disjoint — straight struct-literal map.
    fn normalize(&self) -> Usage {
        Usage {
            input: self.input_tokens,
            output: self.output_tokens,
            cache_read: self.cache_read_input_tokens,
            cache_write: self.cache_creation_input_tokens,
            reasoning: 0,
        }
    }
}

/// Map Anthropic's `stop_reason` string onto the normalized [`StopReason`].
fn map_stop_reason(raw: Option<&str>) -> StopReason {
    match raw {
        Some("end_turn") => StopReason::EndTurn,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("stop_sequence") => StopReason::StopSequence,
        Some(other) => StopReason::Other(other.to_string()),
        None => StopReason::EndTurn,
    }
}

/// Decode a non-streaming Messages response.
fn decode_response(body: &[u8]) -> Result<ChatResponse, ProviderError> {
    let wire: WireResponse = serde_json::from_slice(body)
        .map_err(|e| ProviderError::Decode(format!("decoding response: {e}")))?;
    let content = wire
        .content
        .iter()
        .filter(|block| block.kind == "text")
        .map(|block| block.text.as_str())
        .collect::<String>();
    Ok(ChatResponse {
        model: wire.model,
        content,
        usage: wire.usage.normalize(),
        stop_reason: map_stop_reason(wire.stop_reason.as_deref()),
    })
}

// --- streaming accumulator --------------------------------------------------

/// Folds Anthropic SSE events into a [`crate::StreamOutcome`]. `input` and the
/// cache buckets come from `message_start`; `output` is **overwritten** from the
/// terminal `message_delta` (it is cumulative on the wire — accumulating it would
/// double-count). Text deltas are forwarded to the caller's sink as they arrive.
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
        let event: StreamEvent = serde_json::from_str(payload)
            .map_err(|e| ProviderError::Decode(format!("decoding stream event: {e}")))?;
        match event.kind.as_str() {
            "message_start" => {
                if let Some(message) = event.message {
                    self.usage = message.usage.normalize();
                }
            }
            "content_block_delta" => {
                if let Some(delta) = event.delta {
                    if let Some(text) = delta.text {
                        if !text.is_empty() {
                            on_delta(TokenDelta { text });
                        }
                    }
                }
            }
            "message_delta" => {
                // `usage.output_tokens` here is the cumulative final count.
                self.usage.output = event.usage.output_tokens;
                if let Some(delta) = event.delta {
                    self.stop_reason = Some(map_stop_reason(delta.stop_reason.as_deref()));
                }
            }
            _ => {}
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

#[derive(Deserialize)]
struct StreamEvent {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    message: Option<StreamMessage>,
    #[serde(default)]
    delta: Option<StreamDelta>,
    #[serde(default)]
    usage: WireUsage,
}

#[derive(Deserialize)]
struct StreamMessage {
    #[serde(default)]
    usage: WireUsage,
}

#[derive(Deserialize)]
struct StreamDelta {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::RecordedTransport;

    // A real (lightly trimmed) non-streaming Messages response.
    const COMPLETION_FIXTURE: &str = r#"{
        "id": "msg_01",
        "type": "message",
        "role": "assistant",
        "model": "claude-opus-4-8",
        "content": [
            {"type": "text", "text": "Hello"},
            {"type": "text", "text": ", world"}
        ],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 100,
            "output_tokens": 25,
            "cache_read_input_tokens": 10,
            "cache_creation_input_tokens": 5
        }
    }"#;

    #[test]
    fn complete_decodes_text_and_disjoint_usage() {
        let adapter = AnthropicAdapter::new(
            RecordedTransport::responding(200, COMPLETION_FIXTURE),
            "sk-test",
        );
        let resp = adapter
            .complete(&ChatRequest::new("claude-opus-4-8").user("hi"))
            .unwrap();
        assert_eq!(resp.content, "Hello, world");
        assert_eq!(resp.model, "claude-opus-4-8");
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
        // Disjoint, no double-count: total == input(100)+out(25)+read(10)+write(5).
        assert_eq!(resp.usage.input, 100);
        assert_eq!(resp.usage.output, 25);
        assert_eq!(resp.usage.cache_read, 10);
        assert_eq!(resp.usage.cache_write, 5);
        assert_eq!(resp.usage.reasoning, 0);
        assert_eq!(resp.usage.total(), 140);
    }

    #[test]
    fn build_request_hoists_system_and_sets_headers() {
        let adapter = AnthropicAdapter::new(
            RecordedTransport::responding(200, COMPLETION_FIXTURE),
            "sk-secret",
        );
        let req = ChatRequest::new("claude-opus-4-8")
            .with_system("top-level rules")
            .push(Role::System, "more rules")
            .with_max_tokens(512)
            .with_temperature(0.3)
            .user("hello");
        let _ = adapter.complete(&req).unwrap();

        let sent = adapter.transport.last_request();
        assert_eq!(sent.url, "https://api.anthropic.com/v1/messages");
        assert!(sent
            .headers
            .iter()
            .any(|(k, v)| k == "x-api-key" && v == "sk-secret"));
        assert!(sent
            .headers
            .iter()
            .any(|(k, v)| k == "anthropic-version" && v == ANTHROPIC_VERSION));

        let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
        assert_eq!(body["max_tokens"], 512);
        assert_eq!(body["temperature"], 0.3);
        // Both system sources folded into the top-level field; messages carry no
        // system role.
        assert_eq!(body["system"], "top-level rules\n\nmore rules");
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn complete_applies_default_max_tokens_when_unset() {
        let adapter =
            AnthropicAdapter::new(RecordedTransport::responding(200, COMPLETION_FIXTURE), "k")
                .with_default_max_tokens(256);
        let _ = adapter
            .complete(&ChatRequest::new("claude-opus-4-8").user("hi"))
            .unwrap();
        let body: serde_json::Value =
            serde_json::from_slice(&adapter.transport.last_request().body).unwrap();
        assert_eq!(body["max_tokens"], 256);
    }

    #[test]
    fn complete_maps_status_codes_to_errors() {
        let auth = AnthropicAdapter::new(
            RecordedTransport::responding(401, r#"{"error":{"message":"bad key"}}"#),
            "k",
        );
        assert!(matches!(
            auth.complete(&ChatRequest::new("m").user("x")),
            Err(ProviderError::Auth)
        ));

        let limited = AnthropicAdapter::new(RecordedTransport::responding(429, "{}"), "k");
        assert!(matches!(
            limited.complete(&ChatRequest::new("m").user("x")),
            Err(ProviderError::RateLimited)
        ));

        let bad = AnthropicAdapter::new(
            RecordedTransport::responding(400, r#"{"error":{"message":"nope"}}"#),
            "k",
        );
        match bad.complete(&ChatRequest::new("m").user("x")) {
            Err(ProviderError::Api { status, message }) => {
                assert_eq!(status, 400);
                assert_eq!(message, "nope");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn complete_rejects_out_of_range_temperature_before_sending() {
        let adapter = AnthropicAdapter::new(RecordedTransport::responding(200, "{}"), "k");
        let req = ChatRequest::new("m").with_temperature(9.0).user("x");
        assert!(matches!(
            adapter.complete(&req),
            Err(ProviderError::Unsupported(_))
        ));
    }

    #[test]
    fn stream_emits_deltas_and_overwrites_cumulative_output() {
        // message_start carries input + cache + an initial output_tokens=1;
        // message_delta carries the *cumulative* final output_tokens=25 and the
        // stop reason. Accumulating output would yield 26 — overwriting yields 25.
        let lines = vec![
            r#"event: message_start"#,
            r#"data: {"type":"message_start","message":{"usage":{"input_tokens":100,"output_tokens":1,"cache_read_input_tokens":10,"cache_creation_input_tokens":5}}}"#,
            r#"event: content_block_delta"#,
            r#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}"#,
            r#"event: content_block_delta"#,
            r#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"lo"}}"#,
            r#"event: message_delta"#,
            r#"data: {"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":25}}"#,
            r#"event: message_stop"#,
            r#"data: {"type":"message_stop"}"#,
        ];
        let adapter = AnthropicAdapter::new(RecordedTransport::streaming(lines, 200), "k");
        let mut text = String::new();
        let outcome = adapter
            .stream(&ChatRequest::new("claude-opus-4-8").user("hi"), &mut |d| {
                text.push_str(&d.text)
            })
            .unwrap();
        assert_eq!(text, "Hello");
        assert_eq!(outcome.stop_reason, StopReason::MaxTokens);
        assert_eq!(outcome.usage.input, 100);
        assert_eq!(outcome.usage.cache_read, 10);
        assert_eq!(outcome.usage.cache_write, 5);
        assert_eq!(outcome.usage.output, 25); // overwritten, not 1+25
        assert_eq!(outcome.usage.total(), 140);
    }
}
