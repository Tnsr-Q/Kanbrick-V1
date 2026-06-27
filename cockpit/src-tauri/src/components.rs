//! Component visualizer IPC (P10.4 #116 + P10.5 #117).
//!
//! `list_components` reads the live component catalogue (`GET /me/components`)
//! through the host auth bridge (ADR-0016): the Bearer is injected from the
//! host-held [`Session`](crate::auth::Session), never from a webview argument.
//! `watch_components` opens a [`tauri::ipc::Channel`] and streams periodic
//! snapshots to the webview off the UI thread (the same Channel primitive the
//! BYO-AI console uses), cancellable via `stop_watching`. Both paths resolve
//! identity host-side; the webview supplies nothing but the channel. The returned
//! [`ComponentStatus`] mirrors `kanbrick-api`'s response 1:1 (and the TS type).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use uuid::Uuid;

use crate::auth::{authed_get, Session};

/// How often the visualizer poll loop refreshes the component snapshot.
const POLL_INTERVAL: Duration = Duration::from_millis(1500);
/// Sub-interval tick so a `stop_watching` is observed promptly between polls.
const POLL_TICK: Duration = Duration::from_millis(150);

/// Per-watch cancel flags keyed by the id returned to the webview. Aliased to keep
/// the `Mutex` field under clippy's `type_complexity` bar (as `ProviderHub` does).
type WatchRegistry = HashMap<Uuid, Arc<AtomicBool>>;

/// One running component's status, mirroring `kanbrick-api`'s `ComponentStatus`.
/// `clearance` is the serialized `ClearanceLevel` (`"L1"`..`"L5"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentStatus {
    pub name: String,
    pub version: String,
    pub active: i64,
    pub completed: u64,
    pub failed: u64,
    pub timed_out: u64,
    pub clearance: String,
}

/// Events streamed to the webview over the Channel (internally tagged on `event`,
/// mirrored by the `ComponentsEvent` union in `src/api.ts`).
#[derive(Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ComponentsEvent {
    /// A fresh snapshot of every component's live counters.
    Snapshot {
        /// The current catalogue.
        components: Vec<ComponentStatus>,
    },
    /// A transient fetch error; the loop keeps polling so it self-heals.
    Error {
        /// Human-readable reason.
        message: String,
    },
    /// The watch was stopped (cancelled by the webview).
    Stopped,
}

/// Host-side registry of live visualizer watches so each can be cancelled. Mirrors
/// `ProviderHub`'s stream registry.
#[derive(Default)]
pub struct VisualizerHub {
    watches: Arc<Mutex<WatchRegistry>>,
}

impl VisualizerHub {
    fn register(&self, id: Uuid, cancel: Arc<AtomicBool>) {
        self.watches
            .lock()
            .expect("visualizer watch lock")
            .insert(id, cancel);
    }

    fn cancel(&self, id: Uuid) {
        if let Some(flag) = self.watches.lock().expect("visualizer watch lock").get(&id) {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

/// Fetch the live component catalogue from the sidecar through the auth bridge.
/// Shared by the one-shot [`list_components`] and the streaming [`watch_components`].
/// A 401 clears the host session so the UI falls back to login.
async fn fetch_components(app: &AppHandle) -> Result<Vec<ComponentStatus>, String> {
    let response = authed_get(app, "/me/components").await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        app.state::<Session>().clear();
        return Err("session expired — please sign in again".to_string());
    }
    if !response.status().is_success() {
        return Err(format!("could not load components ({})", response.status()));
    }
    response
        .json::<Vec<ComponentStatus>>()
        .await
        .map_err(|e| format!("unexpected components response: {e}"))
}

/// `invoke('list_components')` — the live component catalogue via `GET /me/components`
/// through the auth bridge. Identity is derived entirely from the host-held token;
/// the webview supplies nothing.
#[tauri::command]
pub async fn list_components(app: AppHandle) -> Result<Vec<ComponentStatus>, String> {
    fetch_components(&app).await
}

/// `invoke('watch_components', { channel })` — stream live component snapshots to
/// `channel` until [`stop_watching`]. Returns a watch id. The poll loop resolves
/// identity host-side on every tick (the auth bridge); the webview passes only the
/// channel. Requires a signed-in session.
#[tauri::command]
pub fn watch_components(
    app: AppHandle,
    session: tauri::State<'_, Session>,
    hub: tauri::State<'_, VisualizerHub>,
    channel: tauri::ipc::Channel<ComponentsEvent>,
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
            let event = match tauri::async_runtime::block_on(fetch_components(&app)) {
                Ok(components) => ComponentsEvent::Snapshot { components },
                Err(message) => ComponentsEvent::Error { message },
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
        let _ = channel.send(ComponentsEvent::Stopped);
        watches
            .lock()
            .expect("visualizer watch lock")
            .remove(&watch_id);
    });

    Ok(watch_id.to_string())
}

/// `invoke('stop_watching', { watch })` — signal the watch loop to stop.
#[tauri::command]
pub fn stop_watching(hub: tauri::State<'_, VisualizerHub>, watch: String) {
    if let Ok(id) = Uuid::parse_str(&watch) {
        hub.cancel(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_status_mirrors_the_api_json() {
        // The exact shape kanbrick-api's `GET /me/components` emits for one component.
        let json = serde_json::json!({
            "name": "valuation",
            "version": "0.1.0",
            "active": 0,
            "completed": 3,
            "failed": 1,
            "timed_out": 0,
            "clearance": "L3"
        });
        let c: ComponentStatus = serde_json::from_value(json).unwrap();
        assert_eq!(c.name, "valuation");
        assert_eq!(c.version, "0.1.0");
        assert_eq!(c.completed, 3);
        assert_eq!(c.failed, 1);
        assert_eq!(c.clearance, "L3");
    }

    #[test]
    fn snapshot_event_serializes_with_its_tag() {
        let event = ComponentsEvent::Snapshot { components: vec![] };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["event"], "snapshot");
        assert!(value["components"].is_array());
        assert_eq!(
            serde_json::to_value(ComponentsEvent::Stopped).unwrap()["event"],
            "stopped"
        );
    }

    #[test]
    fn cancel_sets_the_registered_flag() {
        let hub = VisualizerHub::default();
        let id = Uuid::new_v4();
        let flag = Arc::new(AtomicBool::new(false));
        hub.register(id, flag.clone());
        hub.cancel(id);
        assert!(flag.load(Ordering::Relaxed));
    }
}
