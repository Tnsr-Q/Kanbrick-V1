# ADR 0017 — BYO-AI egress: per-tenant provider-host allowlist + DLP; core stays no-egress

- **Status:** Accepted
- **Date:** 2026-06-25
- **Context:** P8.6 (#98), **Phase 8 — Upstream De-Risk** (#79), L5 Cockpit
  program (#77). Reconciles req 1 (BYO-AI) with the repo's zero-network ethos
  (ADR-0003/0006); builds on ADR-0008 (NetworkPolicy) and ADR-0010 (DLP).
- **Deciders:** operator (egress posture is an operator decision) + P8 de-risk
  agent.

## Context

The system is ADR-locked **zero-external-dependency / no-network** (ADR-0003/0006
— `codegraph` is AST-only by default precisely to honour this). BYO-AI requires
outbound HTTPS to Claude / OpenAI / Cerebras — a genuine one-way door the current
ADRs forbid by default. The operator chose a **per-tenant provider-host allowlist
+ Ironclaw DLP**, with the **core staying no-egress**.

## Decision

1. **The core (CP / mesh / graph) stays no-egress.** ADR-0003/0006 are unchanged
   for everything except the provider boundary.
2. **Egress only from `kanbrick-providers` (P9).** Outbound HTTPS is permitted
   **only** from the provider layer, to a **per-tenant** allowlist of provider
   hosts (`api.anthropic.com`, `api.openai.com`, `api.cerebras.ai`, …).
3. **Per-tenant allowlist data model.** The allowlist lives as a per-tenant
   `(:ProviderAllowlist)` in the central CP graph behind the ADR-0015 queue; it is
   edited by an L4 / L5 lead through the ScopeGrants-style dual gate; it is
   **default-deny** (an un-allowlisted host is refused).
4. **DLP gates every send.** Ironclaw DLP (ADR-0010) checks the
   `(data-class → provider)` pair before the send; every send is audited (reuse
   `AuditLog`).

## Probe evidence

This is primarily a design ADR; its enforcing mechanisms are proven elsewhere —
the **DLP default-deny gate** in `probes/rbac-overlay` (ADR-0010) and **per-tenant
config placement** in ADR-0015. **P9.6** is where the real outbound call lands;
ADR-0017 + ADR-0010 + ADR-0015 are its prerequisites.

## Network policy

The deployed-CP Kubernetes **NetworkPolicy delta** from ADR-0008: ADR-0008
confined the executor off public ingress and limited CP↔executor to ClusterIP.
ADR-0017 **adds** an egress policy permitting **only the providers pod** to reach
the allowlisted provider hosts (FQDN / CIDR `:443`), while **core pods stay
egress-denied**. Defense-in-depth: the app-layer allowlist + DLP is the authority;
the NetworkPolicy is the backstop. Consistent with ADR-0008's confinement model.

## Alternatives considered

- **Global egress.** Rejected: breaks the no-network core invariant and risks
  cross-tenant leakage.
- **A single firm-wide allowlist.** Rejected: per-tenant isolation is required.
- **Proxy / NetworkPolicy enforcement only.** Rejected: the app-layer DLP must be
  the authority; the network policy alone cannot reason about data class.

## Consequences

- The no-egress core is preserved everywhere except the provider boundary;
  per-tenant isolation holds.
- P9.6 builds on this (pairs with ADR-0010); the egress NetworkPolicy is added to
  `deploy/k8s/` when a CP is deployed.
