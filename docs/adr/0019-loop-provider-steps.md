# ADR 0019 — Loop provider steps: host-injected key, model-only `provider_ref`

- **Status:** Accepted
- **Date:** 2026-06-29
- **Context:** Phase 11 (Skill/Loop Ecosystem), slice **P11.4** — per-step
  provider/agent selection for the loop run engine (ADR-0013). Builds on ADR-0009
  (provider-key custody, Stronghold/keychain), ADR-0012 (skill model), ADR-0016
  (host-authoritative IPC identity), ADR-0017 (BYO-AI egress: core no-egress; only
  the providers layer egresses, behind the P9.6 gate), and the P9.1–P9.6 provider
  substrate (`kanbrick-providers`/`kanbrick-egress`).
- **Deciders:** P11 agent + **operator** (HITL — per-step credential/identity flow
  is a one-way security door; the operator chose *skill-bound provider steps* +
  *seam-only, no-network* this session).

## Context

P11.3 shipped the loop run engine: a `(:LoopStep)` is `(skill_name, scope_id)`,
authorized at run time by `ScopeGrants::authorize_skill`, then run as a WASM guest on
the mesh `Scheduler`. P11.4 makes the step **polymorphic** — a step may instead run an
**LLM completion** on a selected provider/model.

The load-bearing security question: *how does a step name a provider without ever
holding a credential or an identity?* The substrate already answers half of it —
provider keys are **retrievable secrets in per-`user_id` custody** (ADR-0009,
`ProviderKeyStore`), the webview/step never sees plaintext, and egress is confined to
the providers layer behind the allowlist+DLP gate (ADR-0017, `kanbrick-egress`). The
run engine runs on the **no-egress CP** (`kanbrick-api`); the only live provider path
today is the cockpit's P9.4 stub.

## Decision

1. **Skill-bound provider steps.** A provider step is `(skill_name, scope_id,
   provider_ref)` and goes through the **same** `authorize_skill` gate as a guest
   step — active+unexpired scope, caller is the grantee, clearance ≥ the skill's
   floor. `provider_ref` (a provider kind + model) **overrides execution** ("run an
   LLM instead of the bound guest") but never the gate. The skill supplies
   authorization + the `ProjectScope`; one uniform run gate covers both step kinds.
   (The guest-policy clearance floor — a guest-execution defense-in-depth — applies
   only to guest steps, since a provider step runs no guest.)

2. **Model-only `provider_ref`; the host injects the key.** `provider_ref` carries
   **only** the provider kind + model — **never** a credential. At run time the host
   resolves the caller's key from `AppState.provider_keys` **by the run's caller
   `user_id`** (the host-authoritative identity, ADR-0002/0016), and injects it into
   the provider. A step can neither supply its own credential nor inject an identity;
   a caller with no saved key for that provider simply fails the step (no fallback).
   The schema stores `provider`/`model` as **opaque strings** in `(:LoopStep)`, so
   `kanbrick-store` stays free of `kanbrick-providers`; the run engine parses + gates.

3. **Seam-only, no live egress (this slice).** The provider is built through an
   injected `ProviderFactory` seam on `AppState` (`with_provider_factory`). The
   default is a **no-network echo factory**; **no `reqwest` ships in core/CI**,
   exactly as ADR-0017 / P9.4 / P9.6 require. At deploy the real factory composes the
   `kanbrick-providers` wire adapters behind the `kanbrick-egress` `GatedTransport`
   (per-tenant allowlist + DLP) over the identical `ChatProvider` interface — so the
   egress gate lives in the deploy-time factory, not the core run engine. The
   security property P11.4 owns (host-injected key, model-only selection) is fully
   wired and tested with zero network.

## Consequences

- **The security invariant is the deliverable and is verified.** A provider step's
  body has no key field (structurally); the key is resolved by caller identity from
  custody and handed to the factory — proven by a recording-factory test that asserts
  the caller's *own saved secret* reached the provider, plus a no-key-fails test.
- **Egress stays ADR-0017-compliant.** The core run engine opens no socket; the real
  call (and its allowlist + DLP gate) is the injected deploy-time factory's job. The
  slice ships the seam + the echo default.
- **Custody source.** The run engine reads `AppState.provider_keys` (the P9.3 sidecar
  custody). The cockpit's P9.4 `ProviderHub` holds a *separate* per-workstation store;
  reconciling/provisioning real keys into the sidecar's custody (and the Stronghold
  backend, ADR-0009) is a deploy concern, deferred — within one process the P9.3
  routes and the run engine share one store (as the tests exercise).
- **Token accounting deferred to P12.** A provider step returns `Usage`; wiring it
  into the priced `kanbrick-tokens` ledger belongs to Phase 12 (Token Tracking).
- **Deferred:** the SKILL.md body as the provider system prompt (the body is not yet
  persisted by the registry); external MCP tool-call steps (P11.5); the cockpit UI
  for authoring/labelling provider steps (P11.6) — the run-and-watch panel already
  shows their live status unchanged.
