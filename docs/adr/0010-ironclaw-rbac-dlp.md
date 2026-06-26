# ADR 0010 — Ironclaw RBAC + DLP as additive-only overlays

- **Status:** Accepted
- **Date:** 2026-06-25
- **Context:** P8.4 (#96), **Phase 8 — Upstream De-Risk** (#79), L5 Cockpit
  program (#77). Builds on ADR-0002 (Ironclaw is a binary) and ADR-0005
  (clearance); feeds ADR-0017 (egress DLP).
- **Deciders:** P8 de-risk agent + operator (HITL — RBAC/DLP composition is a
  one-way door).

## Context

`kanbrick-auth` already uses Ironclaw **primitives** (`jsonwebtoken`, `argon2`),
not the binary (ADR-0002 / Phase 2). The Cockpit now wants Ironclaw's **RBAC +
DLP** for two things:

- **(a) RBAC** — roles that can only *restrict* clearance, never elevate, reading
  the existing `FirmContext.roles` (`pub roles: Vec<String>`,
  `kanbrick-core/src/context.rs`). There must be **no second role store**.
- **(b) DLP** — gate which provider a tenant's data may be sent to, used at the
  P9.6 provider boundary (pairs with ADR-0017).

## Probe evidence

**Library-vs-binary outcome: binary.** Ironclaw has no library target (ADR-0002 /
Phase 2); a fresh inspection of `crates/ironclaw/Cargo.toml` was blocked by the
403 submodule clone in this session (see ADR-0014 /
[`docs/probes/p8.1-upstream-compat-matrix.md`](../probes/p8.1-upstream-compat-matrix.md)).
So the de-risked outcome is **port the pattern**, not depend on the crate.

The ported pattern is proven by the throwaway spike `probes/rbac-overlay` (std
only, built + tested here on stable, **6 tests pass**):

- `clearance_order_matches_kanbrick_core` — mirrors `ClearanceLevel`
  (`L1 < … < L5`, derived `Ord`).
- `role_restricts_clearance` — an L4 caller tagged `contractor` (ceiling L2) is
  capped at L2.
- `role_can_never_elevate` — a role whose ceiling is L5 does **not** raise an L2
  caller; adding roles is monotonically non-increasing.
- `restricted_call_denied_allowed_passes` — the same caller is denied an L3 action
  while restricted, and passes without the role.
- `dlp_blocks_disallowed_pair_allows_allowed` — a non-allowlisted
  (data-class → provider) pair is refused **even for L5** (DLP is orthogonal to
  clearance).
- `dlp_is_default_deny` — only explicitly allowlisted pairs send.

**Mechanism:** a role maps to an *optional clearance ceiling*; effective clearance
= base lowered by `min` over applicable ceilings → it can only ever narrow. DLP is
a default-deny `(DataClass, Provider)` allowlist. Because the spike mirrors the
real `ClearanceLevel` / `FirmContext.roles`, the logic transfers verbatim onto the
real types.

## Decision

1. **RBAC is a restrict-only overlay over `FirmContext.roles`.** No parallel role
   store. A role can only impose a clearance *ceiling*; the composition is `min`,
   never `max`, so a role can never elevate. This is enforced by construction — the
   overlay has no path that raises clearance.
2. **The overlay composes after clearance resolve, before the action.** Production
   binds the spike's logic to `ClearanceLevel` and `ClearanceScope`
   (`kanbrick-auth/src/scope.rs`): resolve clearance, lower it by the role overlay,
   then gate the action / provider send.
3. **DLP is a default-deny `(data-class → provider)` gate** evaluated before any
   send, orthogonal to clearance. It is the app-layer authority used at the P9.6
   boundary (ADR-0017).

## Alternatives considered

- **Run the Ironclaw binary as a sidecar.** Rejected: heavier, no library, and we
  need only the *pattern* — a thin overlay over types we already own.
- **A role → clearance map that can elevate.** Rejected: violates the core
  invariant; clearance comes from authenticated identity, roles may only narrow it.
- **Network-layer DLP only.** Kept as defense-in-depth in ADR-0017, but the
  app-layer DLP is the authority (the network policy is a backstop).

## Consequences

- The overlay is restrict-only by construction and reads the one role list that
  already exists; used program-wide.
- P9.6's DLP send-gate builds directly on this; it pairs with ADR-0017's per-tenant
  allowlist.
- The spike (`probes/rbac-overlay`) is a throwaway, excluded from the workspace;
  the production overlay lands with P9.6.
