//! Kanbrick L5 "Cockpit" — Tauri v2 host shell.
//!
//! P7.1 (#87) stood up the empty window. P7.2 (#88) bundles `kanbrick-api` as a
//! managed sidecar. P7.3 (#89) adds `login`/`logout` + host-side JWT custody.
//! P7.4 (#90, ADR-0016) adds the IPC auth bridge: authenticated calls attach the
//! Bearer from `Session`, never from the webview. P7.5 (#91) renders the `/me`
//! identity panel. Still ahead: P7.6 (#92) — headless CI e2e.
//!
//! The desktop is a *client* of the finished `HTTP → Auth → Mesh → Guest → Graph`
//! spine — it re-implements none of it.

mod auth;
mod components;
mod providers;
mod sidecar;

use auth::Session;
use providers::ProviderHub;
use sidecar::SidecarSupervisor;
use tauri::Manager;

/// Build and run the Cockpit desktop application.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(SidecarSupervisor::default())
        .manage(Session::default())
        .manage(ProviderHub::default())
        .invoke_handler(tauri::generate_handler![
            sidecar::sidecar_status,
            auth::login,
            auth::logout,
            auth::session_status,
            auth::session_refresh,
            auth::me,
            components::list_components,
            providers::save_provider_key,
            providers::list_provider_keys,
            providers::stream_completion,
            providers::cancel_completion,
        ])
        .setup(|app| {
            // Spawn + health-gate the API sidecar; readiness is pushed to the
            // webview via the `sidecar-status` event.
            sidecar::launch(app.handle().clone());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the Kanbrick Cockpit")
        .run(|app_handle, event| match event {
            // Last window closed (or app exit requested): kill the sidecar so no
            // orphaned kanbrick-api process survives the desktop.
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit => {
                if let Some(supervisor) = app_handle.try_state::<SidecarSupervisor>() {
                    supervisor.shutdown();
                }
            }
            _ => {}
        });
}
