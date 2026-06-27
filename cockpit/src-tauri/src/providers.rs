//! BYO-AI streaming over the Tauri Channel API + host-side key custody (P9.4, #104).
//!
//! The webview sends only `{ provider, model, prompt }` — **never a key**. The host
//! resolves the caller's provider key from custody (the P9.3 `ProviderKeyStore`),
//! builds a [`ChatProvider`], and streams [`TokenDelta`]s token-by-token to the
//! webview over a [`tauri::ipc::Channel`] (the right primitive for high-frequency
//! host→webview streaming). Identity and the secret stay host-authoritative
//! (ADR-0016): the key is read host-side and never crosses the IPC boundary
//! outward — `GET`-style key listing returns metadata only, exactly like the P9.3
//! API routes.
//!
//! **Custody backend.** This holds an in-memory [`InMemoryKeyStore`]; per ADR-0009
//! the durable cockpit-side enclave is IOTA Stronghold (native `libsodium`), wired
//! behind the same `ProviderKeyStore` trait. **Provider backend.** P9.4 verifies
//! headless with no live network, so the streamed provider is the cancel-aware
//! [`EchoStreamProvider`] stub; the real P9.2 adapters plug in at P9.6 (behind the
//! ADR-0017 egress gate) through the identical `ChatProvider` interface — the
//! resolved key would be handed to that adapter.
//!
//! **Tenancy.** The cockpit is a single-user, per-workstation control plane
//! (ADR-0015), so custody is namespaced by a fixed [`WORKSTATION_USER`]; the
//! multi-user path keys on the JWT `user_id` and lands with multi-tenant P14.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kanbrick_providers::{
    ChatProvider, ChatRequest, ChatResponse, InMemoryKeyStore, KeyMetadata, ProviderError,
    ProviderKeyStore, ProviderKind, Role, StopReason, StreamOutcome, TokenDelta, Usage,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Session;

/// Single-user per-workstation custody namespace (ADR-0015). Multi-user keying on
/// the JWT `user_id` arrives with P14.
const WORKSTATION_USER: Uuid = Uuid::nil();

/// Pace the stub stream so tokens render incrementally and a cancel has a window.
const STREAM_TICK: Duration = Duration::from_millis(20);

/// Per-stream cancel flags, keyed by the id returned to the webview. Aliased to
/// keep the `Mutex` field under clippy's `type_complexity` bar.
type StreamRegistry = HashMap<Uuid, Arc<AtomicBool>>;

/// Host-side BYO-AI state: provider-key custody + live-stream cancel registry.
#[derive(Default)]
pub struct ProviderHub {
    keys: InMemoryKeyStore,
    streams: Arc<Mutex<StreamRegistry>>,
}

impl ProviderHub {
    /// Resolve the plaintext secret for `provider` host-side (never returned to the
    /// webview). Errors if the caller has saved no key for that provider.
    fn resolve_secret(&self, provider: ProviderKind) -> Result<String, String> {
        let metas = self
            .keys
            .list(WORKSTATION_USER)
            .map_err(|e| e.to_string())?;
        let meta = metas
            .into_iter()
            .find(|m| m.provider == provider)
            .ok_or_else(|| format!("no API key saved for {provider} — add one first"))?;
        self.keys
            .get_secret(WORKSTATION_USER, meta.id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "key not found in custody".to_string())
    }

    fn register(&self, id: Uuid, cancel: Arc<AtomicBool>) {
        self.streams
            .lock()
            .expect("stream registry lock")
            .insert(id, cancel);
    }

    fn cancel(&self, id: Uuid) {
        if let Some(flag) = self.streams.lock().expect("stream registry lock").get(&id) {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

/// What the webview sends to start a completion. Deliberately has **no key field**.
#[derive(Debug, Deserialize)]
pub struct CompletionRequest {
    /// Which provider to use.
    pub provider: ProviderKind,
    /// Provider-specific model id.
    pub model: String,
    /// The user prompt.
    pub prompt: String,
    /// Optional system prompt.
    #[serde(default)]
    pub system: Option<String>,
}

/// Events streamed to the webview over the Channel (internally tagged on `event`,
/// mirrored by the `StreamEvent` union in `src/api.ts`).
#[derive(Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StreamEvent {
    /// One incremental chunk of assistant text.
    Delta {
        /// The chunk.
        text: String,
    },
    /// Terminal success: the final usage + stop reason.
    Done {
        /// Normalized token usage for the whole stream.
        usage: Usage,
        /// Why generation stopped (`end_turn` / `max_tokens` / …).
        stop_reason: String,
    },
    /// Terminal failure talking to the provider.
    Error {
        /// Human-readable reason.
        message: String,
    },
    /// The stream was cancelled by the webview.
    Cancelled,
}

/// `invoke('save_provider_key', { provider, label, secret })` — store a key in
/// host-side custody. The secret crosses **inbound** only; it is never returned
/// (the response is metadata). Requires a signed-in session.
#[tauri::command]
pub fn save_provider_key(
    session: tauri::State<'_, Session>,
    hub: tauri::State<'_, ProviderHub>,
    provider: ProviderKind,
    label: String,
    secret: String,
) -> Result<KeyMetadata, String> {
    require_session(&session)?;
    hub.keys
        .put(WORKSTATION_USER, provider, &label, &secret)
        .map_err(|e| e.to_string())
}

/// `invoke('list_provider_keys')` — metadata for the caller's saved keys (never
/// the secrets). Requires a signed-in session.
#[tauri::command]
pub fn list_provider_keys(
    session: tauri::State<'_, Session>,
    hub: tauri::State<'_, ProviderHub>,
) -> Result<Vec<KeyMetadata>, String> {
    require_session(&session)?;
    hub.keys.list(WORKSTATION_USER).map_err(|e| e.to_string())
}

/// `invoke('stream_completion', { channel, request })` — resolve the selected
/// provider's key host-side, then stream deltas to `channel`. Returns a stream id
/// for [`cancel_completion`]. The webview passes no key.
#[tauri::command]
pub fn stream_completion(
    session: tauri::State<'_, Session>,
    hub: tauri::State<'_, ProviderHub>,
    channel: tauri::ipc::Channel<StreamEvent>,
    request: CompletionRequest,
) -> Result<String, String> {
    require_session(&session)?;

    // Resolve the secret host-side. The P9.4 stub does not call out, but resolving
    // it here proves the key is available host-side and never crosses to the
    // webview; the P9.6 adapter is constructed from `_secret`.
    let _secret = hub.resolve_secret(request.provider)?;

    let stream_id = Uuid::new_v4();
    let cancel = Arc::new(AtomicBool::new(false));
    hub.register(stream_id, cancel.clone());
    let streams = hub.streams.clone();

    let provider = EchoStreamProvider {
        kind: request.provider,
        cancel: cancel.clone(),
    };
    let chat = build_chat_request(&request);

    // Stream off the UI thread (the stub paces itself with sleeps), mirroring the
    // sidecar health gate's std-thread pattern.
    std::thread::spawn(move || {
        let result = {
            let mut sink = |delta: TokenDelta| {
                let _ = channel.send(StreamEvent::Delta { text: delta.text });
            };
            provider.stream(&chat, &mut sink)
        };
        let event = match result {
            Ok(outcome) => {
                if cancel.load(Ordering::Relaxed) {
                    StreamEvent::Cancelled
                } else {
                    StreamEvent::Done {
                        usage: outcome.usage,
                        stop_reason: stop_reason_label(&outcome.stop_reason),
                    }
                }
            }
            Err(e) => StreamEvent::Error {
                message: e.to_string(),
            },
        };
        let _ = channel.send(event);
        streams
            .lock()
            .expect("stream registry lock")
            .remove(&stream_id);
    });

    Ok(stream_id.to_string())
}

/// `invoke('cancel_completion', { stream })` — signal the stream to stop. The stub
/// breaks its loop cleanly; the P9.6 transport checks the same flag to drop a live
/// connection.
#[tauri::command]
pub fn cancel_completion(hub: tauri::State<'_, ProviderHub>, stream: String) {
    if let Ok(id) = Uuid::parse_str(&stream) {
        hub.cancel(id);
    }
}

fn require_session(session: &Session) -> Result<(), String> {
    if session.token().is_some() {
        Ok(())
    } else {
        Err("not signed in".to_string())
    }
}

fn build_chat_request(request: &CompletionRequest) -> ChatRequest {
    let mut chat = ChatRequest::new(request.model.as_str()).user(request.prompt.as_str());
    if let Some(system) = &request.system {
        chat = chat.with_system(system.as_str());
    }
    chat
}

fn stop_reason_label(reason: &StopReason) -> String {
    match reason {
        StopReason::EndTurn => "end_turn".to_string(),
        StopReason::MaxTokens => "max_tokens".to_string(),
        StopReason::StopSequence => "stop_sequence".to_string(),
        StopReason::Other(other) => other.clone(),
    }
}

/// The last user message text in a request, or empty.
fn last_user_text(request: &ChatRequest) -> String {
    request
        .messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, Role::User))
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

fn word_count(text: &str) -> u64 {
    text.split_whitespace().count() as u64
}

/// A no-network [`ChatProvider`] that streams the prompt back word-by-word,
/// honoring a cancel flag between chunks. Stands in for the P9.2 adapters until
/// P9.6 wires real egress; it exercises the Channel + cancellation end-to-end.
struct EchoStreamProvider {
    kind: ProviderKind,
    cancel: Arc<AtomicBool>,
}

impl ChatProvider for EchoStreamProvider {
    fn kind(&self) -> ProviderKind {
        self.kind
    }

    fn complete(&self, request: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        let text = last_user_text(request);
        let usage = Usage {
            input: word_count(&text),
            output: word_count(&text),
            ..Usage::default()
        };
        Ok(ChatResponse {
            model: request.model.clone(),
            content: text,
            usage,
            stop_reason: StopReason::EndTurn,
        })
    }

    fn stream(
        &self,
        request: &ChatRequest,
        on_delta: &mut dyn FnMut(TokenDelta),
    ) -> Result<StreamOutcome, ProviderError> {
        let text = last_user_text(request);
        let input = word_count(&text);
        let mut output = 0u64;
        let mut cancelled = false;
        for chunk in text.split_inclusive(' ') {
            if self.cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            on_delta(TokenDelta {
                text: chunk.to_string(),
            });
            output = output.saturating_add(1);
            std::thread::sleep(STREAM_TICK);
        }
        let stop_reason = if cancelled {
            StopReason::Other("cancelled".to_string())
        } else {
            StopReason::EndTurn
        };
        Ok(StreamOutcome {
            usage: Usage {
                input,
                output,
                ..Usage::default()
            },
            stop_reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(prompt: &str) -> ChatRequest {
        ChatRequest::new("stub-model").user(prompt)
    }

    #[test]
    fn stub_streams_prompt_word_by_word() {
        let provider = EchoStreamProvider {
            kind: ProviderKind::OpenAI,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let mut chunks: Vec<String> = Vec::new();
        let outcome = provider
            .stream(&req("hello there world"), &mut |d| chunks.push(d.text))
            .unwrap();
        assert_eq!(chunks.concat(), "hello there world");
        assert_eq!(outcome.usage.output, 3);
        assert_eq!(outcome.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn stub_honors_cancellation_before_streaming() {
        let cancel = Arc::new(AtomicBool::new(true)); // pre-cancelled
        let provider = EchoStreamProvider {
            kind: ProviderKind::Anthropic,
            cancel,
        };
        let mut chunks: Vec<String> = Vec::new();
        let outcome = provider
            .stream(&req("never sent"), &mut |d| chunks.push(d.text))
            .unwrap();
        assert!(chunks.is_empty());
        assert_eq!(
            outcome.stop_reason,
            StopReason::Other("cancelled".to_string())
        );
    }

    #[test]
    fn resolve_secret_errors_without_a_saved_key() {
        let hub = ProviderHub::default();
        assert!(hub.resolve_secret(ProviderKind::OpenAI).is_err());
    }

    #[test]
    fn save_then_resolve_is_host_side_round_trip() {
        let hub = ProviderHub::default();
        hub.keys
            .put(WORKSTATION_USER, ProviderKind::Cerebras, "k", "sk-secret")
            .unwrap();
        assert_eq!(
            hub.resolve_secret(ProviderKind::Cerebras).unwrap(),
            "sk-secret"
        );
        // A different provider still misses.
        assert!(hub.resolve_secret(ProviderKind::OpenAI).is_err());
    }

    #[test]
    fn cancel_sets_the_registered_flag() {
        let hub = ProviderHub::default();
        let id = Uuid::new_v4();
        let flag = Arc::new(AtomicBool::new(false));
        hub.register(id, flag.clone());
        hub.cancel(id);
        assert!(flag.load(Ordering::Relaxed));
    }
}
