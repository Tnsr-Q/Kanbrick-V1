// Typed wrappers around the Cockpit's Tauri IPC surface. Keeping every `invoke`
// in one place gives P7.4/P7.5 a single seam to extend. The JWT lives host-side
// (P7.3) — nothing here ever handles the raw token.
import { invoke, Channel } from "@tauri-apps/api/core";
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

/**
 * Validate the held token against `GET /me` through the host auth bridge
 * (P7.4 / ADR-0016). Unlike `getSessionStatus`, this detects an expired token —
 * a 401 clears the host session. Use this on startup / after a reload.
 */
export const sessionRefresh = (): Promise<SessionState> =>
  invoke<SessionState>("session_refresh");

/** The signed-in user's identity (mirror of the Rust `Identity` / `MeResponse`). */
export type Identity = { email: string; clearance: string; roles: string[] };

/** `GET /me` through the host auth bridge (P7.4 / ADR-0016). */
export const me = (): Promise<Identity> => invoke<Identity>("me");

export const login = (email: string, password: string): Promise<void> =>
  invoke<void>("login", { email, password });

export const logout = (): Promise<void> => invoke<void>("logout");

// ── Component visualizer (P10.4, #116) ───────────────────────────────────────

/** Mirror of the Rust `ComponentStatus` (kanbrick-api `GET /me/components`). */
export type ComponentStatus = {
  name: string;
  version: string;
  active: number;
  completed: number;
  failed: number;
  timed_out: number;
  clearance: string;
};

/** The live component catalogue via the host auth bridge (ADR-0016). */
export const listComponents = (): Promise<ComponentStatus[]> =>
  invoke<ComponentStatus[]>("list_components");

// ── BYO-AI providers (P9.4, #104) ────────────────────────────────────────────

/** Mirror of `kanbrick_providers::ProviderKind` (serde lowercase). */
export type ProviderKind = "anthropic" | "openai" | "cerebras";

/** Mirror of `kanbrick_providers::KeyMetadata` — deliberately carries no secret. */
export type KeyMetadata = {
  id: string;
  provider: ProviderKind;
  label: string;
  created_at: number;
};

/** Mirror of `kanbrick_providers::Usage` (mutually disjoint token buckets). */
export type Usage = {
  input: number;
  output: number;
  cache_read: number;
  cache_write: number;
  reasoning: number;
};

/** Mirror of the Rust `StreamEvent` (serde internally-tagged on `event`). */
export type StreamEvent =
  | { event: "delta"; text: string }
  | { event: "done"; usage: Usage; stop_reason: string }
  | { event: "error"; message: string }
  | { event: "cancelled" };

/** What the webview sends — never a key; the host resolves it from custody. */
export type CompletionRequest = {
  provider: ProviderKind;
  model: string;
  prompt: string;
  system?: string;
};

/**
 * Store a provider key in host-side custody. The secret crosses **inbound** only;
 * the response is metadata — no secret is ever returned (ADR-0016).
 */
export const saveProviderKey = (
  provider: ProviderKind,
  label: string,
  secret: string,
): Promise<KeyMetadata> =>
  invoke<KeyMetadata>("save_provider_key", { provider, label, secret });

/** Metadata for the caller's saved keys (never the secrets). */
export const listProviderKeys = (): Promise<KeyMetadata[]> =>
  invoke<KeyMetadata[]>("list_provider_keys");

/**
 * Start a streaming completion. `onEvent` fires for each `delta` and once for the
 * terminal `done` | `error` | `cancelled`. Resolves to a stream id for
 * {@link cancelCompletion}. The key is NEVER passed here — the host resolves it
 * from custody, so it cannot leak through the IPC contract (ADR-0016).
 */
export const streamCompletion = (
  request: CompletionRequest,
  onEvent: (event: StreamEvent) => void,
): Promise<string> => {
  const channel = new Channel<StreamEvent>();
  channel.onmessage = onEvent;
  return invoke<string>("stream_completion", { channel, request });
};

/** Cancel an in-flight stream by id. */
export const cancelCompletion = (stream: string): Promise<void> =>
  invoke<void>("cancel_completion", { stream });
