//! Shared test doubles for the wire adapters (`#[cfg(test)]` only).
//!
//! [`RecordedTransport`] is a deterministic [`HttpTransport`] that replays a
//! captured response (or captured SSE lines) and records the request it was
//! handed, so the P9.2 codecs are exercised end-to-end with **zero network** —
//! the same recorded-fixture discipline the real P9.6 transport will be validated
//! against once egress exists.

use std::sync::Mutex;

use crate::wire::{HttpRequest, HttpResponse, HttpTransport};
use crate::ProviderError;

/// A canned [`HttpTransport`]: returns a fixed response (or replays fixed SSE
/// lines) and remembers the last request for assertions.
pub struct RecordedTransport {
    response: Option<HttpResponse>,
    stream_lines: Vec<String>,
    stream_status: u16,
    last_request: Mutex<Option<HttpRequest>>,
}

impl RecordedTransport {
    /// A transport whose `send` returns `status` + `body`.
    pub fn responding(status: u16, body: &str) -> Self {
        RecordedTransport {
            response: Some(HttpResponse {
                status,
                body: body.as_bytes().to_vec(),
            }),
            stream_lines: Vec::new(),
            stream_status: status,
            last_request: Mutex::new(None),
        }
    }

    /// A transport whose `send_streaming` replays `lines` (raw SSE lines) and
    /// returns `status`.
    pub fn streaming(lines: Vec<&str>, status: u16) -> Self {
        RecordedTransport {
            response: None,
            stream_lines: lines.into_iter().map(|s| s.to_string()).collect(),
            stream_status: status,
            last_request: Mutex::new(None),
        }
    }

    /// The request the adapter last handed to the transport.
    pub fn last_request(&self) -> HttpRequest {
        self.last_request
            .lock()
            .unwrap()
            .clone()
            .expect("a request was sent to the transport")
    }
}

impl HttpTransport for RecordedTransport {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, ProviderError> {
        *self.last_request.lock().unwrap() = Some(request.clone());
        self.response
            .clone()
            .ok_or_else(|| ProviderError::Transport("no recorded response".to_string()))
    }

    fn send_streaming(
        &self,
        request: &HttpRequest,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<u16, ProviderError> {
        *self.last_request.lock().unwrap() = Some(request.clone());
        for line in &self.stream_lines {
            on_line(line);
        }
        Ok(self.stream_status)
    }
}
