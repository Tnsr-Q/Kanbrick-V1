//! Messenger IPC (P10.3, #115).
//!
//! Send messages via `POST /me/messenger/send` and stream the live message log via
//! a [`tauri::ipc::Channel`] (polling `GET /me/messenger/log` off the UI thread —
//! the same pattern the visualizer uses), cancellable via `stop_messages`. The
//! webview sends only message *content* + scope; the `actor` is stamped host-side
//! server-side from the validated identity (ADR-0002/0016), and the host injects
//! the Bearer — nothing identity-bearing crosses the IPC outward. The collaborative
//! whiteboard rides this same stream: strokes are ordinary messages scoped to a
//! `whiteboard` group, so they propagate with no extra backend surface.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use uuid::Uuid;

use crate::auth::{authed_get, authed_post, Session};

/// How often the messenger poll loop refreshes the message log.
const POLL_INTERVAL: Duration = Duration::from_millis(1000);
/// Sub-interval tick so a `stop_messages` is observed promptly between polls.
const POLL_TICK: Duration = Duration::from_millis(150);

/// Per-watch cancel flags keyed by the id returned to the webview. Aliased to keep
/// the `Mutex` field under clippy's `type_complexity` bar (as `ProviderHub` does).
type WatchRegistry = HashMap<Uuid, Arc<AtomicBool>>;

/// Who a message is addressed to. Mirrors `kanbrick-core::abi::MessengerScope`
/// (serde internally-tagged on `kind`) and the TS `MessengerScope`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessengerScope {
    /// Firm-wide.
    Public,
    /// Addressed to a named group (an addressing label at P10.1, not an ACL).
    Group {
        /// The group name.
        name: String,
    },
}

/// One message, mirroring `kanbrick-api`'s `MessengerEvent` response (and the TS type).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessengerMessage {
    /// Host-authoritative sender (stamped server-side; never from the webview).
    pub actor: String,
    /// The message body (for a whiteboard stroke, a JSON-encoded stroke).
    pub text: String,
    /// Who it is addressed to.
    pub scope: MessengerScope,
}

/// Events streamed to the webview over the Channel (internally tagged on `event`,
/// mirrored by the `MessagesEvent` union in `src/api.ts`).
#[derive(Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum MessagesEvent {
    /// A fresh snapshot of the message log (oldest→newest).
    Snapshot {
        /// The current log.
        messages: Vec<MessengerMessage>,
    },
    /// A transient fetch error; the loop keeps polling so it self-heals.
    Error {
        /// Human-readable reason.
        message: String,
    },
    /// The watch was stopped (cancelled by the webview).
    Stopped,
}

/// Host-side registry of live message watches so each can be cancelled. Mirrors
/// `ProviderHub`'s stream registry.
#[derive(Default)]
pub struct MessengerHub {
    watches: Arc<Mutex<WatchRegistry>>,
}

impl MessengerHub {
    fn register(&self, id: Uuid, cancel: Arc<AtomicBool>) {
        self.watches
            .lock()
            .expect("messenger watch lock")
            .insert(id, cancel);
    }

    fn cancel(&self, id: Uuid) {
        if let Some(flag) = self.watches.lock().expect("messenger watch lock").get(&id) {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

/// Body for `POST /me/messenger/send`. The webview supplies content + scope only —
/// never `actor`, which the server stamps from the host-authoritative identity.
#[derive(Serialize)]
struct SendBody {
    text: String,
    scope: MessengerScope,
}

/// Fetch the current message log from the sidecar through the auth bridge. Shared by
/// the one-shot [`message_log`] and the streaming [`watch_messages`]. A 401 clears
/// the host session so the UI falls back to login.
async fn fetch_messages(app: &AppHandle) -> Result<Vec<MessengerMessage>, String> {
    let response = authed_get(app, "/me/messenger/log").await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        app.state::<Session>().clear();
        return Err("session expired — please sign in again".to_string());
    }
    if !response.status().is_success() {
        return Err(format!("could not load messages ({})", response.status()));
    }
    response
        .json::<Vec<MessengerMessage>>()
        .await
        .map_err(|e| format!("unexpected messenger response: {e}"))
}

/// `invoke('send_message', { text, scope })` — post a message via the P10.1 route.
/// The webview passes content + scope; the server stamps the host-authoritative
/// `actor`, which the response echoes back.
#[tauri::command]
pub async fn send_message(
    app: AppHandle,
    text: String,
    scope: MessengerScope,
) -> Result<MessengerMessage, String> {
    let response = authed_post(&app, "/me/messenger/send", &SendBody { text, scope }).await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        app.state::<Session>().clear();
        return Err("session expired — please sign in again".to_string());
    }
    if !response.status().is_success() {
        return Err(format!("could not send message ({})", response.status()));
    }
    response
        .json::<MessengerMessage>()
        .await
        .map_err(|e| format!("unexpected messenger response: {e}"))
}

/// `invoke('message_log')` — the current message log (one-shot, oldest→newest).
#[tauri::command]
pub async fn message_log(app: AppHandle) -> Result<Vec<MessengerMessage>, String> {
    fetch_messages(&app).await
}

/// `invoke('watch_messages', { channel })` — stream the live message log to `channel`
/// until [`stop_messages`]. Returns a watch id. The poll loop resolves identity
/// host-side on every tick (the auth bridge); the webview passes only the channel.
/// Requires a signed-in session.
#[tauri::command]
pub fn watch_messages(
    app: AppHandle,
    session: tauri::State<'_, Session>,
    hub: tauri::State<'_, MessengerHub>,
    channel: tauri::ipc::Channel<MessagesEvent>,
) -> Result<String, String> {
    if session.token().is_none() {
        return Err("not signed in".to_string());
    }

    let watch_id = Uuid::new_v4();
    let cancel = Arc::new(AtomicBool::new(false));
    hub.register(watch_id, cancel.clone());
    let watches = hub.watches.clone();

    // Poll off the UI thread, bridging to the async auth-bridge fetch with
    // `block_on` (the std-thread streaming pattern the BYO-AI console uses).
    std::thread::spawn(move || {
        while !cancel.load(Ordering::Relaxed) {
            let event = match tauri::async_runtime::block_on(fetch_messages(&app)) {
                Ok(messages) => MessagesEvent::Snapshot { messages },
                Err(message) => MessagesEvent::Error { message },
            };
            // A send error means the webview dropped the channel — stop polling.
            if channel.send(event).is_err() {
                break;
            }
            // Wait one interval in short ticks so a stop is observed promptly.
            let mut waited = Duration::ZERO;
            while waited < POLL_INTERVAL && !cancel.load(Ordering::Relaxed) {
                std::thread::sleep(POLL_TICK);
                waited += POLL_TICK;
            }
        }
        let _ = channel.send(MessagesEvent::Stopped);
        watches
            .lock()
            .expect("messenger watch lock")
            .remove(&watch_id);
    });

    Ok(watch_id.to_string())
}

/// `invoke('stop_messages', { watch })` — signal the watch loop to stop.
#[tauri::command]
pub fn stop_messages(hub: tauri::State<'_, MessengerHub>, watch: String) {
    if let Ok(id) = Uuid::parse_str(&watch) {
        hub.cancel(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_round_trips_the_api_json() {
        // The exact shape kanbrick-api's messenger routes emit for one message.
        let json = serde_json::json!({
            "actor": "elena.ruiz@kanbrick.com",
            "text": "hello team",
            "scope": { "kind": "public" }
        });
        let m: MessengerMessage = serde_json::from_value(json).unwrap();
        assert_eq!(m.actor, "elena.ruiz@kanbrick.com");
        assert_eq!(m.text, "hello team");
        assert!(matches!(m.scope, MessengerScope::Public));
    }

    #[test]
    fn group_scope_tags_with_its_name() {
        let scope = MessengerScope::Group {
            name: "whiteboard".to_string(),
        };
        let value = serde_json::to_value(&scope).unwrap();
        assert_eq!(value["kind"], "group");
        assert_eq!(value["name"], "whiteboard");
    }

    #[test]
    fn snapshot_event_serializes_with_its_tag() {
        let event = MessagesEvent::Snapshot { messages: vec![] };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["event"], "snapshot");
        assert!(value["messages"].is_array());
        assert_eq!(
            serde_json::to_value(MessagesEvent::Stopped).unwrap()["event"],
            "stopped"
        );
    }

    #[test]
    fn cancel_sets_the_registered_flag() {
        let hub = MessengerHub::default();
        let id = Uuid::new_v4();
        let flag = Arc::new(AtomicBool::new(false));
        hub.register(id, flag.clone());
        hub.cancel(id);
        assert!(flag.load(Ordering::Relaxed));
    }
}
