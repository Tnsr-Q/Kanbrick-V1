// Typed wrappers around the Cockpit's Tauri IPC surface. Keeping every `invoke`
// in one place gives P7.4/P7.5 a single seam to extend. The JWT lives host-side
// (P7.3) — nothing here ever handles the raw token.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** Mirror of the Rust `SidecarStatus` (serde internally-tagged on `state`). */
export type SidecarStatus =
  | { state: "starting" }
  | { state: "ready"; base_url: string }
  | { state: "failed"; reason: string };

export type SessionState = { authenticated: boolean };

export const getSidecarStatus = (): Promise<SidecarStatus> =>
  invoke<SidecarStatus>("sidecar_status");

export const onSidecarStatus = (
  cb: (status: SidecarStatus) => void,
): Promise<UnlistenFn> =>
  listen<SidecarStatus>("sidecar-status", (event) => cb(event.payload));

export const getSessionStatus = (): Promise<SessionState> =>
  invoke<SessionState>("session_status");

export const login = (email: string, password: string): Promise<void> =>
  invoke<void>("login", { email, password });

export const logout = (): Promise<void> => invoke<void>("logout");
