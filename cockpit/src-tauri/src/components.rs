//! Component visualizer IPC (P10.4, #116).
//!
//! `list_components` reads the live component catalogue (`GET /me/components`)
//! through the host auth bridge (ADR-0016): the Bearer is injected from the
//! host-held [`Session`](crate::auth::Session), never from a webview argument, and
//! the webview supplies nothing. The returned [`ComponentStatus`] mirrors
//! `kanbrick-api`'s response 1:1 (and the TS `ComponentStatus`).

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::auth::{authed_get, Session};

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

/// `invoke('list_components')` — the live component catalogue via `GET /me/components`
/// through the auth bridge. Identity is derived entirely from the host-held token;
/// the webview supplies nothing. A 401 clears the session so the UI returns to login.
#[tauri::command]
pub async fn list_components(app: AppHandle) -> Result<Vec<ComponentStatus>, String> {
    let response = authed_get(&app, "/me/components").await?;
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

#[cfg(test)]
mod tests {
    use super::ComponentStatus;

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
}
