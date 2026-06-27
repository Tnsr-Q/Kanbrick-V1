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
//! P7.4 (#90, ADR-0016) adds the **IPC auth bridge** ([`authed_get`]): every
//! authenticated host→sidecar call attaches the Bearer from [`Session`], never from
//! a webview argument, so identity stays host-authoritative across the IPC boundary.

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

    pub(crate) fn clear(&self) {
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

/// `invoke('session_status')` — whether a token is currently held host-side
/// (cheap, in-memory; does not validate it). Use `session_refresh` to validate.
#[tauri::command]
pub fn session_status(session: tauri::State<'_, Session>) -> SessionState {
    SessionState {
        authenticated: session.is_authenticated(),
    }
}

// ── IPC auth bridge (P7.4 / ADR-0016) ───────────────────────────────────────

/// The single authenticated host→sidecar call path.
///
/// Attaches the Bearer token from [`Session`] — **never** from a webview argument.
/// This is the IPC analogue of the mesh propagating `FirmContext` from the
/// validated token (ADR-0002): the webview cannot supply or forge identity. Every
/// future authenticated command (P7.5 `/me`, providers, loops, …) goes through
/// this (or a sibling) rather than minting its own request.
pub(crate) async fn authed_get(app: &AppHandle, path: &str) -> Result<reqwest::Response, String> {
    let base_url = app
        .state::<SidecarSupervisor>()
        .base_url()
        .ok_or_else(|| "the local API is still starting".to_string())?;
    let token = app
        .state::<Session>()
        .token()
        .ok_or_else(|| "not signed in".to_string())?;
    reqwest::Client::new()
        .get(format!("{base_url}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("could not reach the local API: {e}"))
}

/// `invoke('session_refresh')` — validate the held token against `GET /me`.
///
/// The host injects the Bearer; the webview supplies nothing. A **401** is the
/// sidecar's authoritative verdict (ADR-0016 §4), so the session is cleared and
/// the UI falls back to login. A non-401 server error or a transport blip is not
/// an auth failure — the session is kept rather than spuriously signing out.
#[tauri::command]
pub async fn session_refresh(app: AppHandle) -> SessionState {
    if app.state::<Session>().token().is_none() {
        return SessionState {
            authenticated: false,
        };
    }
    match authed_get(&app, "/me").await {
        Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED => {
            app.state::<Session>().clear();
            SessionState {
                authenticated: false,
            }
        }
        _ => SessionState {
            authenticated: true,
        },
    }
}

/// The signed-in user's identity, mirroring `kanbrick-api`'s `MeResponse`.
/// `clearance` is the serialized `ClearanceLevel` (`"L1"`..`"L5"`).
#[derive(Serialize, Deserialize)]
pub struct Identity {
    pub email: String,
    pub clearance: String,
    pub roles: Vec<String>,
}

/// `invoke('me')` — the signed-in user's identity via `GET /me` through the auth
/// bridge (ADR-0016). Identity is derived entirely from the host-held token; the
/// webview supplies nothing. A 401 clears the session so the UI returns to login.
#[tauri::command]
pub async fn me(app: AppHandle) -> Result<Identity, String> {
    let response = authed_get(&app, "/me").await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        app.state::<Session>().clear();
        return Err("session expired — please sign in again".to_string());
    }
    if !response.status().is_success() {
        return Err(format!("could not load identity ({})", response.status()));
    }
    response
        .json::<Identity>()
        .await
        .map_err(|e| format!("unexpected identity response: {e}"))
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
