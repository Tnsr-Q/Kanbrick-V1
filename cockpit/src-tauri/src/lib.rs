//! Kanbrick L5 "Cockpit" — Tauri v2 host shell.
//!
//! P7.1 (issue #87) stands up the empty desktop: one window rendering the React
//! splash. Later slices layer on the real path without re-implementing any of it:
//!
//! * P7.2 — bundle `kanbrick-api` as a managed sidecar (spawn → `/health` → teardown)
//! * P7.3 — `login()` command + JWT custody in Tauri secure storage
//! * P7.4 — rehydrate `FirmContext` on every IPC command (ADR-0016 auth bridge)
//! * P7.5 — render the live `/me` identity panel
//!
//! This file deliberately holds no identity or business logic — the desktop is a
//! *client* of the finished `HTTP → Auth → Mesh → Guest → Graph` spine.

/// Build and run the Cockpit desktop application.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running the Kanbrick Cockpit");
}
