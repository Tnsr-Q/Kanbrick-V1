# ADR 0016 — Cockpit IPC auth contract: FirmContext stays host-authoritative across the Tauri boundary

- **Status:** Accepted
- **Date:** 2026-06-25
- **Context:** Phase 7 (L5 Cockpit), slice P7.4 (#90), building on P7.2 (sidecar,
  #88) and P7.3 (host-side JWT custody, #89). Extends the host-authoritative
  identity invariant of ADR-0002 across the new **Tauri IPC boundary**, and reuses
  the sidecar's existing clearance gate (`require_clearance`) and `ApiError`
  401/403 as the sole authority.
- **Deciders:** P7 agent + **operator** (the IPC auth contract is a one-way door —
  it fixes where identity lives and who is allowed to assert it for the whole
  Cockpit program; this ADR records that decision).

## Context

The Cockpit adds a surface the firm OS did not have before: a **React webview**
talking to a **Rust host** over Tauri IPC, with the host driving the bundled
`kanbrick-api` sidecar. ADR-0002 established that identity is **host-authoritative**
— a caller never injects identity into a payload; the mesh propagates `FirmContext`
from the *validated JWT*, and the guest reads it via `kbk_ctx_*`. ADR-0007 §3
reaffirmed it (the composed `ProjectScope` is handed to the host, never the
guest's `_firm_context`).

The webview is the **least-trusted** surface in the Cockpit: it runs JS, has
devtools, and will later render third-party/agent content. The IPC boundary is
therefore a new trust boundary. Without a contract, a buggy or compromised webview
could try to (a) hold the token in `localStorage`, (b) pass a `user`/`clearance`/
`firm` argument to a command, or (c) have the host decode JWT claims and make its
own authz decisions — each of which would erode the ADR-0002 invariant.

The sidecar already validates the JWT → `FirmContext` and gates every protected
route with `require_clearance` (`ApiError` 401/403). The Cockpit must **defer to
it**, not duplicate it.

## Decision

1. **The JWT lives host-side only.** Custody is `auth::Session` (in-memory for
   P7.3; durable backing is ADR-0009 / P8.2). The token is never returned to the
   webview, never written to web storage, and never logged. The webview learns
   only `authenticated: bool`.

2. **Webview→host commands never accept identity.** No command takes a `token`,
   `user`, `email`-as-identity, `clearance`, `firm`, or `roles` argument. The
   webview's only authority is "act as the currently-signed-in user." Any identity
   hint from the webview is ignored. (`login` takes `email`/`password` as
   *credentials* to obtain a token — not as an identity assertion.)

3. **One bridge for every authenticated call.** All authenticated host→sidecar
   requests go through `auth::authed_get` (and its future siblings), which attaches
   `Authorization: Bearer <Session token>`. There is no other authenticated path.
   This is the IPC analogue of the mesh propagating `FirmContext` from the
   validated token (ADR-0002).

4. **The sidecar is the authority; the host does not re-interpret it.** The host
   does **not** decode JWT claims to gate the UI or commands. It forwards the
   token and lets `kanbrick-api` validate it (`require_clearance`) and rehydrate
   `FirmContext`. A **401** is the sidecar's authoritative verdict: the host clears
   the session and reports `authenticated: false`, and the webview falls back to
   login. `ApiError` 401/403 are surfaced, never re-interpreted into a local
   decision.

5. **"Authenticated" means the token validates, not merely "present".**
   `session_refresh` round-trips `GET /me` through the bridge, so a stale/expired
   token (8 h TTL) is detected on a webview reload rather than trusted blindly. A
   transport blip is **not** an auth failure — the session is preserved (the UI
   only refreshes once the sidecar is health-green, so the sidecar is up).

## Consequences

- A buggy or compromised webview **cannot forge identity or escalate clearance**:
  the worst it can do is act as the already-signed-in user (which it could anyway)
  or call commands that the sidecar's clearance gates reject server-side.
- The token never crosses the IPC boundary outward, so it cannot leak via
  `localStorage`, devtools, or a serialized command result.
- Enforcement stays **single-sited**: `require_clearance` on the sidecar remains
  the only authz point. The Cockpit adds no parallel authorization to drift from
  it. This mirrors ADR-0002 across the new boundary.
- **Every future authenticated command** (P7.5 `/me`, P9 providers, P11 loops, P12
  token usage, …) MUST use the bridge. Adding an identity-bearing parameter to a
  command is a contract violation, caught in review.
- Durable token storage is independent of this contract and is deferred to
  ADR-0009 (P8.2): wherever the token rests, the bridge and the no-identity-from-
  webview rule are unchanged.

## Alternatives rejected

- **Token in the webview / `localStorage`, JS sets `Authorization`.** Exposes the
  token to the least-trusted surface; leaks via devtools/XSS; the webview becomes
  an identity-asserting caller — the exact thing ADR-0002 forbids.
- **Webview passes `FirmContext` / `user_id` to the host.** Caller-supplied
  identity; rejected on the same ADR-0002 grounds (cf. ADR-0007 §3 declining to
  inject `_firm_context`).
- **Host decodes JWT claims to gate the UI/commands.** Duplicates the sidecar's
  authority and risks drift between two authz implementations. The host needs only
  *presence + validity*, which it learns authoritatively from the sidecar's
  200/401.
