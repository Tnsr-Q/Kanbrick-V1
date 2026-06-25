//! Sidecar supervisor (P7.2, issue #88).
//!
//! The Cockpit runs the existing `kanbrick-api` binary as a Tauri **sidecar**
//! (`bundle.externalBin`) — this is the per-workstation control plane of ADR-0015.
//! Nothing about the API is re-implemented; the desktop spawns it, waits for
//! `GET /health` to go green, publishes the base URL to the webview, and tears the
//! process down on exit.
//!
//! The health probe is deliberately dependency-free (a raw HTTP/1.0 `GET /health`
//! over `std::net::TcpStream`) so the readiness contract is unit-testable without a
//! running desktop. Identity is **not** involved here — readiness is pure infra;
//! the JWT/`FirmContext` bridge lands in P7.3/P7.4.

use std::net::{SocketAddr, TcpStream};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

/// Health-poll budget: 80 attempts × 250 ms ≈ 20 s before declaring failure.
const HEALTH_ATTEMPTS: u32 = 80;
const HEALTH_DELAY: Duration = Duration::from_millis(250);

/// Event name the webview subscribes to for sidecar state transitions.
const STATUS_EVENT: &str = "sidecar-status";

/// State of the bundled `kanbrick-api` sidecar, mirrored to the webview.
///
/// Serializes internally-tagged, e.g. `{"state":"ready","base_url":"http://…"}`,
/// matching the `SidecarStatus` union in `src/App.tsx`.
#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum SidecarStatus {
    /// Spawned; waiting for `GET /health` to return 200.
    Starting,
    /// Healthy and serving on `base_url`.
    Ready { base_url: String },
    /// Failed to spawn or never became healthy.
    Failed { reason: String },
}

/// Owns the sidecar child handle and the latest status. Managed as Tauri state.
pub struct SidecarSupervisor {
    child: Mutex<Option<CommandChild>>,
    status: Mutex<SidecarStatus>,
}

impl Default for SidecarSupervisor {
    fn default() -> Self {
        Self {
            child: Mutex::new(None),
            status: Mutex::new(SidecarStatus::Starting),
        }
    }
}

impl SidecarSupervisor {
    fn store_child(&self, child: CommandChild) {
        *self.child.lock().expect("sidecar child lock") = Some(child);
    }

    fn set_status(&self, status: SidecarStatus) {
        *self.status.lock().expect("sidecar status lock") = status;
    }

    fn snapshot(&self) -> SidecarStatus {
        self.status.lock().expect("sidecar status lock").clone()
    }

    /// Kill the sidecar if it is running. Idempotent — safe to call on every
    /// exit event, and after a failed start.
    pub fn shutdown(&self) {
        if let Some(child) = self.child.lock().expect("sidecar child lock").take() {
            let _ = child.kill();
        }
    }
}

/// `invoke('sidecar_status')` — current snapshot, so the webview can render the
/// right state even if it subscribes after the transition event fired.
#[tauri::command]
pub fn sidecar_status(supervisor: tauri::State<'_, SidecarSupervisor>) -> SidecarStatus {
    supervisor.snapshot()
}

/// Spawn the sidecar and drive it to readiness. Called from Tauri `setup`.
pub fn launch(app: AppHandle) {
    if let Err(reason) = start(&app) {
        fail(&app, reason);
    }
}

fn start(app: &AppHandle) -> Result<(), String> {
    // Keep the firm DB + guest-asset volume inside the OS app-data dir so the
    // desktop never depends on the process working directory or a system path.
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("no app data dir: {e}"))?;
    std::fs::create_dir_all(&data_dir).map_err(|e| format!("cannot create data dir: {e}"))?;
    let asset_dir = data_dir.join("assets");
    std::fs::create_dir_all(&asset_dir).map_err(|e| format!("cannot create asset dir: {e}"))?;
    let db_path = data_dir.join("firm.db");

    // Bind an ephemeral port so per-workstation CPs never collide.
    let port = free_port().map_err(|e| format!("cannot allocate a port: {e}"))?;

    let args = vec![
        "--port".to_string(),
        port.to_string(),
        "--db".to_string(),
        db_path.to_string_lossy().into_owned(),
        "--asset-dir".to_string(),
        asset_dir.to_string_lossy().into_owned(),
    ];

    let command = app
        .shell()
        .sidecar("kanbrick-api")
        .map_err(|e| format!("sidecar `kanbrick-api` is not bundled: {e}"))?
        .args(args);

    let (mut rx, child) = command
        .spawn()
        .map_err(|e| format!("failed to spawn the kanbrick-api sidecar: {e}"))?;
    app.state::<SidecarSupervisor>().store_child(child);

    // Forward sidecar stdout/stderr to the host log so a bad start is diagnosable.
    tauri::async_runtime::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stdout(bytes) | CommandEvent::Stderr(bytes) => {
                    eprint!("[kanbrick-api] {}", String::from_utf8_lossy(&bytes));
                }
                CommandEvent::Error(err) => eprintln!("[kanbrick-api] spawn error: {err}"),
                CommandEvent::Terminated(payload) => eprintln!(
                    "[kanbrick-api] terminated: code={:?} signal={:?}",
                    payload.code, payload.signal
                ),
                _ => {}
            }
        }
    });

    // Health-gate off the UI thread, then publish readiness (or failure).
    let app = app.clone();
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    std::thread::spawn(move || {
        if wait_for_health(addr, HEALTH_ATTEMPTS, HEALTH_DELAY) {
            publish(
                &app,
                SidecarStatus::Ready {
                    base_url: format!("http://127.0.0.1:{port}"),
                },
            );
        } else {
            // Never leave a half-started process behind.
            app.state::<SidecarSupervisor>().shutdown();
            let secs = (HEALTH_ATTEMPTS as u64 * HEALTH_DELAY.as_millis() as u64) / 1000;
            fail(
                &app,
                format!("kanbrick-api did not pass GET /health on 127.0.0.1:{port} within {secs}s"),
            );
        }
    });

    Ok(())
}

fn publish(app: &AppHandle, status: SidecarStatus) {
    app.state::<SidecarSupervisor>().set_status(status.clone());
    let _ = app.emit(STATUS_EVENT, status);
}

fn fail(app: &AppHandle, reason: String) {
    eprintln!("[cockpit] sidecar failed: {reason}");
    publish(app, SidecarStatus::Failed { reason });
}

/// Reserve an ephemeral localhost port (bound, then released for the child).
fn free_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

/// Block until `GET /health` returns `200`, or `attempts` are exhausted.
fn wait_for_health(addr: SocketAddr, attempts: u32, delay: Duration) -> bool {
    for _ in 0..attempts {
        if probe_once(addr, delay) {
            return true;
        }
        std::thread::sleep(delay);
    }
    false
}

/// One health probe: a raw HTTP/1.0 `GET /health`, true iff the status line is 200.
fn probe_once(addr: SocketAddr, timeout: Duration) -> bool {
    use std::io::{Read, Write};

    let Ok(mut stream) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));

    let request = b"GET /health HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
    if stream.write_all(request).is_err() {
        return false;
    }

    let mut buf = [0u8; 32];
    match stream.read(&mut buf) {
        Ok(n) if n > 0 => {
            let head = String::from_utf8_lossy(&buf[..n]);
            head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.0 200")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A throwaway HTTP server that answers every connection with a fixed status.
    fn stub_server(status_line: &'static str) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut scratch = [0u8; 256];
                let _ = s.read(&mut scratch);
                let body = "{\"status\":\"healthy\"}";
                let resp = format!(
                    "{status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });
        addr
    }

    #[test]
    fn health_passes_when_server_returns_200() {
        let addr = stub_server("HTTP/1.1 200 OK");
        assert!(wait_for_health(addr, 20, Duration::from_millis(50)));
    }

    #[test]
    fn health_fails_when_server_never_returns_200() {
        let addr = stub_server("HTTP/1.1 503 Service Unavailable");
        assert!(!wait_for_health(addr, 4, Duration::from_millis(25)));
    }

    #[test]
    fn health_fails_on_a_closed_port() {
        // free_port() binds then drops, leaving the port closed.
        let port = free_port().unwrap();
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        assert!(!wait_for_health(addr, 3, Duration::from_millis(25)));
    }
}
