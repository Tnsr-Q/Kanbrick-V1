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

/** Mirror of the Rust `ComponentKind` (serde snake_case). Three component kinds
 * surface uniformly in the visualizer (P10.7, #119). */
export type ComponentKind = "guest" | "sidecar" | "service";

/** Mirror of the Rust `ComponentStatus` (kanbrick-api `GET /me/components`). Only
 * guests carry live invocation counters; sidecars/services report zeroes. */
export type ComponentStatus = {
  name: string;
  version: string;
  active: number;
  completed: number;
  failed: number;
  timed_out: number;
  clearance: string;
  kind: ComponentKind;
};

/** The live component catalogue via the host auth bridge (ADR-0016). */
export const listComponents = (): Promise<ComponentStatus[]> =>
  invoke<ComponentStatus[]>("list_components");

/** Mirror of the Rust `ComponentsEvent` (serde internally-tagged on `event`). */
export type ComponentsEvent =
  | { event: "snapshot"; components: ComponentStatus[] }
  | { event: "error"; message: string }
  | { event: "stopped" };

/**
 * Stream live component snapshots over a Channel (P10.5). `onEvent` fires for each
 * `snapshot` (and on `error` / `stopped`). Resolves to a watch id for
 * {@link stopWatching}. Identity is host-side — the webview passes only the channel.
 */
export const watchComponents = (
  onEvent: (event: ComponentsEvent) => void,
): Promise<string> => {
  const channel = new Channel<ComponentsEvent>();
  channel.onmessage = onEvent;
  return invoke<string>("watch_components", { channel });
};

/** Stop a live component watch by id. */
export const stopWatching = (watch: string): Promise<void> =>
  invoke<void>("stop_watching", { watch });

// ── Messenger + whiteboard (P10.3, #115) ─────────────────────────────────────

/** Mirror of the Rust `MessengerScope` (serde internally-tagged on `kind`). */
export type MessengerScope =
  | { kind: "public" }
  | { kind: "group"; name: string };

/** Mirror of `kanbrick-api`'s `MessengerEvent` (and the Rust `MessengerMessage`). */
export type MessengerMessage = {
  actor: string;
  text: string;
  scope: MessengerScope;
};

/** Mirror of the Rust `MessagesEvent` (serde internally-tagged on `event`). */
export type MessagesEvent =
  | { event: "snapshot"; messages: MessengerMessage[] }
  | { event: "error"; message: string }
  | { event: "stopped" };

/**
 * Post a message via the P10.1 route (`POST /me/messenger/send`). The webview sends
 * only content + scope; the host injects the Bearer and the server stamps the
 * host-authoritative `actor` (echoed back). No identity crosses outward (ADR-0016).
 */
export const sendMessage = (
  text: string,
  scope: MessengerScope,
): Promise<MessengerMessage> =>
  invoke<MessengerMessage>("send_message", { text, scope });

/** The current message log, oldest→newest (one-shot). */
export const messageLog = (): Promise<MessengerMessage[]> =>
  invoke<MessengerMessage[]>("message_log");

/**
 * Stream the live message log over a Channel (P10.3). `onEvent` fires for each
 * `snapshot` (and on `error` / `stopped`). Resolves to a watch id for
 * {@link stopMessages}. The webview passes only the channel.
 */
export const watchMessages = (
  onEvent: (event: MessagesEvent) => void,
): Promise<string> => {
  const channel = new Channel<MessagesEvent>();
  channel.onmessage = onEvent;
  return invoke<string>("watch_messages", { channel });
};

/** Stop a live message watch by id. */
export const stopMessages = (watch: string): Promise<void> =>
  invoke<void>("stop_messages", { watch });

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

// ── Loop run-and-watch (P11.7) ───────────────────────────────────────────────

/** Mirror of the Rust `LoopStepView` (kanbrick-api `LoopStepDto`). */
export type LoopStepView = {
  position: number;
  skill_name: string;
  scope_id: string;
};

/** Mirror of the Rust `LoopSummary` (kanbrick-api `LoopDto`). */
export type LoopSummary = {
  loop_id: string;
  name: string;
  owner: string;
  created_at: string;
  steps: LoopStepView[];
};

/** Mirror of the Rust `RunStepView` (kanbrick-api `RunStepDto`). `status` is one of
 * `pending|running|completed|denied|failed|timed_out`; `detail` carries the reason
 * for a denied/failed step. */
export type RunStepView = {
  position: number;
  skill_name: string;
  scope_id: string;
  status: string;
  detail?: string | null;
};

/** Mirror of the Rust `RunView` (kanbrick-api `RunDto`). `status` is
 * `running|completed|failed`. */
export type RunView = {
  run_id: string;
  loop_id: string;
  started_at: string;
  status: string;
  steps: RunStepView[];
};

/** Mirror of the Rust `RunEvent` (serde internally-tagged on `event`). */
export type RunEvent =
  | { event: "snapshot"; run: RunView }
  | { event: "error"; message: string }
  | { event: "stopped" };

/** The caller's loops via the host auth bridge (ADR-0016). */
export const listLoops = (): Promise<LoopSummary[]> =>
  invoke<LoopSummary[]>("list_loops");

/**
 * Run a loop (`POST /me/loops/{id}/run`). The webview passes only the loop id +
 * optional input; the host injects the Bearer and the server gates each step at run
 * time. Resolves to the initial run state (carrying the `run_id` to watch).
 */
export const runLoop = (loopId: string, input?: unknown): Promise<RunView> =>
  invoke<RunView>("run_loop", { loopId, input: input ?? null });

/**
 * Stream a run's per-step status over a Channel (P11.7) until it reaches a terminal
 * state. `onEvent` fires for each `snapshot` (and on `error` / `stopped`). Resolves
 * to a watch id for {@link stopRunWatch}. The webview passes only the run id + channel.
 */
export const watchRun = (
  runId: string,
  onEvent: (event: RunEvent) => void,
): Promise<string> => {
  const channel = new Channel<RunEvent>();
  channel.onmessage = onEvent;
  return invoke<string>("watch_run", { runId, channel });
};

/** Stop a live run watch by id. */
export const stopRunWatch = (watch: string): Promise<void> =>
  invoke<void>("stop_run_watch", { watch });

// ── Skill authoring + library + loop builder (P11.6) ─────────────────────────

/** Mirror of the Rust `SkillVersion` (kanbrick-api `SkillVersionRecord`). One
 * published edition of a skill; `min_clearance` is `"L1"`..`"L5"`. */
export type SkillVersion = {
  skill_name: string;
  version: string;
  guest: string;
  min_clearance: string;
  description: string;
  source: string;
  created_at: string;
  seq: number;
  /** Publish trust-gate state (P11.8): "pending"|"approved"|"rejected"; null/absent = pending. */
  review_status?: string | null;
  reviewed_by?: string | null;
  reviewed_at?: string | null;
};

/** A reviewer's decision on a published edition (P11.8). */
export type ReviewDecision = "approve" | "reject";

/** Mirror of the Rust `BoundSkill` (kanbrick-api `SkillDto`) — a skill edition bound
 * onto a scope, with its run-time clearance floor. */
export type BoundSkill = {
  id: string;
  name: string;
  scope_id: string;
  guest: string;
  required_clearance: string;
};

/** Mirror of the Rust `GrantedScopeView` (kanbrick-api `GrantedScopeDto`). `status`
 * is `pending|active|expired|revoked`. */
export type GrantedScopeView = {
  id: string;
  project: string;
  requester: string;
  granted_by: string;
  granted_persons: string[];
  granted_companies: string[];
  expires_at?: string | null;
  status: string;
};

/** A provider step's model selection (P11.4) — model only, never a credential. */
export type ProviderRef = { provider: ProviderKind; model: string };

/** An MCP tool step's selection (P11.5) — tool + optional static args, never identity. */
export type ToolRef = { tool: string; args?: unknown };

/** One step to author (mirror of the Rust `StepSpec` / kanbrick-api `StepInput`). A
 * step is at most one kind: guest (neither ref), provider (`provider_ref`), or MCP
 * tool (`tool_ref`). */
export type LoopStepSpec = {
  skill_name: string;
  scope_id: string;
  provider_ref?: ProviderRef;
  tool_ref?: ToolRef;
};

/**
 * Publish a `SKILL.md` edition into the versioned catalogue (`POST /me/skills`). The
 * webview sends only the manifest text; the host injects the Bearer and the server
 * host-stamps the author. A malformed manifest rejects with the server's message.
 */
export const publishSkill = (skillMd: string): Promise<SkillVersion> =>
  invoke<SkillVersion>("publish_skill", { skillMd });

/** Browse the catalogue — the latest edition of every skill (`GET /me/skills`). */
export const listSkills = (): Promise<SkillVersion[]> =>
  invoke<SkillVersion[]>("list_skills");

/** Every published edition of one skill, oldest→newest (`GET /me/skills/{name}`). */
export const skillHistory = (name: string): Promise<SkillVersion[]> =>
  invoke<SkillVersion[]>("skill_history", { name });

/**
 * Bind a published edition onto a scope (`POST /me/scopes/{id}/skills`). `version`
 * omitted binds the latest. Gated server-side on scope ownership.
 */
export const bindSkill = (
  scopeId: string,
  skillName: string,
  version?: string,
): Promise<BoundSkill> =>
  invoke<BoundSkill>("bind_skill", { scopeId, skillName, version: version ?? null });

/** The caller's active grants for a project (`GET /me/scopes?project=…`), to pick a
 * scope to bind onto or reference in a loop step. */
export const listScopes = (project: string): Promise<GrantedScopeView[]> =>
  invoke<GrantedScopeView[]>("list_scopes", { project });

/**
 * Author a loop (`POST /me/loops`). Each step names a bound skill + scope and is
 * guest, provider, or MCP-tool; the server validates each. The created loop then
 * appears in the {@link listLoops} picker for run-and-watch.
 */
export const createLoop = (
  name: string,
  steps: LoopStepSpec[],
): Promise<LoopSummary> => invoke<LoopSummary>("create_loop", { name, steps });

// ── Skill-publish trust gate / reviewer queue (P11.8) ────────────────────────

/**
 * The pending publish-review queue (`GET /me/skill-reviews`). L4-gated server-side;
 * a non-reviewer's call rejects, which the UI uses to hide the reviewer panel.
 */
export const listSkillReviews = (): Promise<SkillVersion[]> =>
  invoke<SkillVersion[]>("list_skill_reviews");

/**
 * Approve or reject a published edition (`POST /me/skill-reviews/{name}/{version}`).
 * Eligibility (reviewer over the author, no self-review) is enforced host-side.
 * Returns the updated edition.
 */
export const reviewSkill = (
  name: string,
  version: string,
  decision: ReviewDecision,
  reason?: string,
): Promise<SkillVersion> =>
  invoke<SkillVersion>("review_skill", {
    name,
    version,
    decision,
    reason: reason ?? null,
  });
