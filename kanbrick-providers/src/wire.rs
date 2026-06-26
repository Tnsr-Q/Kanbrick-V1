//! The HTTP transport seam for the provider wire adapters (P9.2, #102).
//!
//! This slice ships the **codec**: encode a [`ChatRequest`](crate::ChatRequest)
//! into each provider's wire JSON, decode the wire JSON back into the normalized
//! types — and, critically, map each provider's differently-shaped token usage
//! onto the disjoint [`Usage`](crate::Usage) buckets without double-counting. The
//! bytes are moved by an injected [`HttpTransport`], so the adapters carry **no
//! HTTP / TLS / async stack** and the double-count-prone mapping is unit-testable
//! against recorded fixtures with zero network.
//!
//! The real TLS-capable transport (a `reqwest` client behind the ADR-0017 egress
//! allowlist + ADR-0010 DLP gate) lands in **P9.6**. Until that gate exists,
//! ADR-0017 forbids core data from leaving the host, so P9.2 deliberately makes
//! no live call: the only [`HttpTransport`] in this crate is the in-test
//! `RecordedTransport`. The seam mirrors `kanbrick-api::http_client` (a
//! `{status, body}` response over a `(method, url, headers, body)` request) so
//! the P9.6 transport wraps that existing client rather than introducing a new
//! HTTP shape.

use crate::ProviderError;

/// An outbound HTTP request, fully formed by an adapter and handed to a transport.
///
/// The adapter owns wire concerns (URL path, auth headers, serialized body); the
/// transport owns only moving the bytes. `headers` are sent verbatim — the
/// transport adds nothing provider-specific.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// HTTP method (always `"POST"` for the chat endpoints).
    pub method: &'static str,
    /// Absolute request URL (`https://host/path`).
    pub url: String,
    /// Header name/value pairs, sent verbatim.
    pub headers: Vec<(String, String)>,
    /// The serialized request body.
    pub body: Vec<u8>,
}

/// A transport's response: the status code plus the raw body bytes.
///
/// Mirrors `kanbrick-api::http_client::HttpResponse` so the P9.6 transport can
/// return it directly. The adapter — not the transport — interprets the status
/// (401 → [`ProviderError::Auth`], 429 → [`ProviderError::RateLimited`], other
/// non-2xx → [`ProviderError::Api`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Raw response body bytes.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Whether the status is in the 2xx success range.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The injected HTTP transport. Object-safe and `Send + Sync` so an adapter that
/// holds `Box<dyn HttpTransport>` is itself a `Send + Sync`
/// [`ChatProvider`](crate::ChatProvider).
///
/// A transport only moves bytes. Connection / TLS / timeout failures map to
/// [`ProviderError::Transport`]; the adapter adds HTTP-status interpretation on
/// top of a returned [`HttpResponse`].
pub trait HttpTransport: Send + Sync {
    /// Perform one request/response round-trip (the `complete` path).
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, ProviderError>;

    /// Perform a streaming request, delivering each raw SSE **line** to `on_line`
    /// as it arrives, and returning the final HTTP status.
    ///
    /// The transport owns line-framing (splitting the `text/event-stream` body on
    /// `\n`); the adapter owns SSE *event semantics* via [`sse_data`]. This split
    /// keeps the part that needs real byte-level IO (buffering chunks into lines)
    /// in the P9.6 transport, while the part this slice must get right (which JSON
    /// field is a token delta, where the final usage lives) stays in the codec and
    /// is exercised by `RecordedTransport`.
    fn send_streaming(
        &self,
        request: &HttpRequest,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<u16, ProviderError>;
}

/// Extract the payload of one Server-Sent-Events `data:` line.
///
/// Returns `Some(payload)` for a `data:` line (with the optional single leading
/// space after the colon stripped per the SSE spec), and `None` for blank lines,
/// comments (`:`-prefixed), and field lines the codec ignores (`event:`, `id:`).
/// The sentinel `data: [DONE]` yields `Some("[DONE]")` — compare against
/// [`SSE_DONE`].
pub fn sse_data(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("data:")?;
    // SSE allows exactly one optional space after the colon.
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// The OpenAI / Cerebras stream terminator payload (`data: [DONE]`).
pub const SSE_DONE: &str = "[DONE]";

/// Validate a request's optional sampling temperature before it is serialized.
///
/// Every provider accepts `0.0..=2.0` and 400s on anything else (or on `NaN`),
/// so the codec rejects it up front as [`ProviderError::Unsupported`] rather than
/// burning a round-trip — the rejection the P9.1 [`ChatRequest`](crate::ChatRequest)
/// doc anticipated. `None` (provider default) is always allowed.
pub(crate) fn check_temperature(temperature: Option<f32>) -> Result<(), ProviderError> {
    match temperature {
        None => Ok(()),
        Some(t) if t.is_nan() || !(0.0..=2.0).contains(&t) => Err(ProviderError::Unsupported(
            format!("temperature {t} out of range [0.0, 2.0]"),
        )),
        Some(_) => Ok(()),
    }
}

/// Map a non-2xx provider HTTP status onto the normalized [`ProviderError`].
///
/// `401`/`403` → [`ProviderError::Auth`], `429` → [`ProviderError::RateLimited`],
/// anything else → [`ProviderError::Api`] carrying the provider's own error
/// message (both Anthropic and OpenAI wrap it as `{"error":{"message":...}}`),
/// falling back to the raw body when that shape is absent.
pub(crate) fn status_error(status: u16, body: &[u8]) -> ProviderError {
    match status {
        401 | 403 => ProviderError::Auth,
        429 => ProviderError::RateLimited,
        _ => ProviderError::Api {
            status,
            message: extract_error_message(body)
                .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned()),
        },
    }
}

/// Pull `error.message` out of a provider error body, if present.
fn extract_error_message(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value
        .get("error")?
        .get("message")?
        .as_str()
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_data_strips_prefix_and_single_space() {
        assert_eq!(sse_data("data: {\"x\":1}"), Some("{\"x\":1}"));
        assert_eq!(sse_data("data:{\"x\":1}"), Some("{\"x\":1}"));
        assert_eq!(sse_data("data:  two-spaces"), Some(" two-spaces"));
        assert_eq!(sse_data("data: [DONE]"), Some(SSE_DONE));
    }

    #[test]
    fn sse_data_ignores_non_data_lines() {
        assert_eq!(sse_data(""), None);
        assert_eq!(sse_data(": comment"), None);
        assert_eq!(sse_data("event: message_start"), None);
        assert_eq!(sse_data("id: 42"), None);
    }

    #[test]
    fn status_error_maps_auth_and_rate_limit() {
        assert!(matches!(status_error(401, b""), ProviderError::Auth));
        assert!(matches!(status_error(403, b""), ProviderError::Auth));
        assert!(matches!(status_error(429, b""), ProviderError::RateLimited));
    }

    #[test]
    fn status_error_extracts_provider_message() {
        let body = br#"{"error":{"type":"invalid_request_error","message":"bad model"}}"#;
        match status_error(400, body) {
            ProviderError::Api { status, message } => {
                assert_eq!(status, 400);
                assert_eq!(message, "bad model");
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    #[test]
    fn status_error_falls_back_to_raw_body() {
        match status_error(500, b"upstream boom") {
            ProviderError::Api { status, message } => {
                assert_eq!(status, 500);
                assert_eq!(message, "upstream boom");
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    #[test]
    fn check_temperature_accepts_in_range_and_none() {
        assert!(check_temperature(None).is_ok());
        assert!(check_temperature(Some(0.0)).is_ok());
        assert!(check_temperature(Some(2.0)).is_ok());
        assert!(check_temperature(Some(0.7)).is_ok());
    }

    #[test]
    fn check_temperature_rejects_out_of_range_and_nan() {
        assert!(matches!(
            check_temperature(Some(-0.1)),
            Err(ProviderError::Unsupported(_))
        ));
        assert!(matches!(
            check_temperature(Some(2.5)),
            Err(ProviderError::Unsupported(_))
        ));
        assert!(matches!(
            check_temperature(Some(f32::NAN)),
            Err(ProviderError::Unsupported(_))
        ));
    }
}
