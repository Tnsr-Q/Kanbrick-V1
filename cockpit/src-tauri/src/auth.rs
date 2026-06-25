//! Session auth bridge (P7.3, issue #89).
//!
//! `login` forwards credentials to the sidecar's `POST /login`, takes the signed
//! JWT, and holds it **host-side, in memory** ([`Session`]). The webview never
//! receives the raw token — it only ever learns `authenticated: bool` — so the JWT
//! is never written to web storage or logs (issue #89 AC). Because the host
//! process outlives a webview reload, the session survives a reload (the panel
//! re-queries [`session_status`]) without the token ever touching plain storage.
//!
//! Durable, cross-*restart* secure custody (OS keychain vs IOTA Stronghold) is the
//! P8.2 / ADR-0009 one-way door and is intentionally NOT decided here. [`Session`]
//! is the seam: a future durable backing implements the same set/clear/token API.
//! P7.4 (ADR-0016) builds on [`Session::token`] to attach the Bearer on every IPC
//! command, host-authoritatively.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::sidecar::SidecarSupervisor;

/// Host-side custody of the session JWT. In-memory for P7.3 (see module docs).
#[derive(Default)]
pub struct Session {
    token: Mutex<Option<String>>,
}

impl Session {
    fn set(&self, token: String) {
        *self.token.lock().expect("session lock") = Some(token);
    }

    fn clear(&self) {
        *self.token.lock().expect("session lock") = None;
    }

    /// The current JWT, if signed in. Used by the IPC auth bridge (P7.4).
    pub fn token(&self) -> Option<String> {
        self.token.lock().expect("session lock").clone()
    }

    fn is_authenticated(&self) -> bool {
        self.token.lock().expect("session lock").is_some()
    }
}

/// Reported to the webview — deliberately never includes the token itself.
#[derive(Serialize)]
pub struct SessionState {
    pub authenticated: bool,
}

/// `invoke('login', { email, password })` — authenticate against the sidecar and
/// take custody of the JWT host-side. Returns a user-facing error on failure.
#[tauri::command]
pub async fn login(app: AppHandle, email: String, password: String) -> Result<(), String> {
    let base_url = app
        .state::<SidecarSupervisor>()
        .base_url()
        .ok_or_else(|| "the local API is still starting — try again in a moment".to_string())?;

    #[derive(Serialize)]
    struct Body<'a> {
        email: &'a str,
        password: &'a str,
    }
    #[derive(Deserialize)]
    struct TokenBody {
        token: String,
    }

    let response = reqwest::Client::new()
        .post(format!("{base_url}/login"))
        .json(&Body {
            email: &email,
            password: &password,
        })
        .send()
        .await
        .map_err(|e| format!("could not reach the local API: {e}"))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err("invalid email or password".to_string());
    }
    if !response.status().is_success() {
        return Err(format!("login failed ({})", response.status()));
    }

    let body: TokenBody = response
        .json()
        .await
        .map_err(|e| format!("unexpected login response: {e}"))?;
    app.state::<Session>().set(body.token);
    Ok(())
}

/// `invoke('logout')` — drop the stored token.
#[tauri::command]
pub fn logout(session: tauri::State<'_, Session>) {
    session.clear();
}

/// `invoke('session_status')` — whether a token is currently held host-side.
#[tauri::command]
pub fn session_status(session: tauri::State<'_, Session>) -> SessionState {
    SessionState {
        authenticated: session.is_authenticated(),
    }
}

#[cfg(test)]
mod tests {
    use super::Session;

    #[test]
    fn session_holds_and_clears_the_token() {
        let session = Session::default();
        assert!(!session.is_authenticated());
        assert_eq!(session.token(), None);

        session.set("jwt.abc.def".to_string());
        assert!(session.is_authenticated());
        assert_eq!(session.token().as_deref(), Some("jwt.abc.def"));

        session.clear();
        assert!(!session.is_authenticated());
        assert_eq!(session.token(), None);
    }
}
