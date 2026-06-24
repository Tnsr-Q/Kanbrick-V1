# ADR 0008 — Control-plane / executor split: horizontal scale-out with host-authoritative identity across the network

- **Status:** Accepted
- **Date:** 2026-06-24
- **Context:** Post-PRD cloud-native mesh upgrades (#68 Track D, #69 Track E,
  #70 Track F, #71 Track G). Retires the single-pod scale-out guardrail of #65
  (`scale-out-prerequisites.md`). Builds on the WASM runtime + host-authoritative
  identity (ADR-0002), the SparrowDB dialect (ADR-0001), the clearance model
  (ADR-0005), the guest registry (#64), and the mesh pressure metrics (#63).
- **Deciders:** Scale-out agent + operator (architecture chosen up front:
  **API/executor split + graph-RPC**, built-ready then implemented across the four
  tracks).

## Context

Kanbrick-V1 shipped single-pod: the embedded SparrowDB graph and the
content-addressed asset registry are single-writer and live on one `ReadWriteOnce`
volume, so `replicas: 1` was a hard correctness guardrail, not a tuning default
(#65). But WASM execution — the CPU-heavy, untrusted part of an invocation — is
exactly the part that benefits from horizontal scale.

The constraint that made naive scale-out unsafe is that **guests are not
stateless**: a guest calls `kbk_query_graph` (clearance-filtered reads, #24) and
`kbk_emit_event` (#27), so an execution replica still needs the graph and the
authoritative caller identity. The hard requirement is ADR-0002's invariant —
**identity is host-authoritative**: a guest can only *read* its `FirmContext` via
`kbk_ctx_*`, never set or forge it, and every graph read is clearance-filtered at
a single audited choke point. Any split must preserve that **across a network
hop**, where a replica is now a separate, independently-compromisable process.

## Decision

Split into two tiers, keeping a single writer:

1. **Control plane (CP)** — one pod (`replicas: 1`, `strategy: Recreate`). Owns the
   embedded SparrowDB graph, the asset registry, **every write**, guest
   activation, and the authoritative identity. Serves the public API and a
   **ClusterIP-only internal RPC surface** (graph read / event emit / asset fetch /
   registry listing). Not scaled.

2. **Executor pool** — a stateless, horizontally-scalable Deployment. Runs guest
   WASM. No store, no JWT, no PVC, no public surface. A guest's graph/event host
   calls are serviced by a remote `HostServices` backend that proxies them **back**
   to the CP's internal RPC surface.

### Host-authoritative identity across the network — capability tokens

The executor must run a guest under the caller's **real** clearance, yet a
compromised executor (or a WASM escape) must not be able to act as a different
identity or read above what the CP authorized. The resolution is a per-invocation
**capability**:

- The CP mints a short-lived, single-use, unguessable capability (two v4 UUIDs ⇒
  244 bits of CSPRNG entropy) bound **server-side** to the authoritative
  `FirmContext`, and forwards the invocation relaying **only** the opaque
  capability. The identity bytes never leave the CP process.
- On a callback, the executor presents the capability; the CP **resolves it
  server-side** to the bound identity and runs the read through the same
  clearance-enforcing `GuardedStore`. The `ctx` shipped to the executor with the
  invocation is read-only state for the guest's `kbk_ctx_*` imports and is **never
  trusted** on a callback.
- A forged or expired capability is a `401` ⇒ the guest's query traps (no data
  leak). The capability is revoked the instant the invocation returns.

This extends ADR-0002's host-authoritative identity across the hop: the executor
relays a bearer it cannot read or forge, and authority is always recovered on the
CP. (Mirrors the same reasoning in ADR-0007: identity stays host-side; only the
minimum needed is handed across a boundary.)

### Transport, confinement, and autoscaling

- **Transport secret.** Both internal surfaces are gated by a shared
  `x-kanbrick-internal-token`, compared in constant time and **failing closed**
  when unset (supplied via the `internal-token` Secret key).
- **In-cluster, plain HTTP.** CP↔executor traffic is between ClusterIP Services,
  never the public ingress. `networkpolicy.yaml` confines the internal port to
  CP↔executor and keeps the executor off all public ingress.
- **KEDA scales the executor only**, on `kanbrick_mesh_pressure_ratio` (#63)
  aggregated across executor pods. The CP has no ScaledObject. **No GPU/DCGM
  trigger** — guests run CPU-only in wasmtime.
- **Registry consistency without shared storage.** Executors replay the
  activated-guest set from the CP on boot and reconcile on a persisted
  registry-generation bump (#69), pulling assets and hot-reloading.
- **Back-compat.** With no executor configured, the CP runs guests in-process —
  byte-for-byte the prior single-pod behaviour.

### Implementation seam (no churn in the dispatch path)

The graph/event host imports were first routed through a `HostServices` trait
(#68, Track D) so the runtime is indifferent to whether the backing is the
in-process `LocalHostServices` or the executor's remote backend. The mesh threads
an optional per-invocation capability to that trait (`invoke_with_cap`, #70); the
hand-rolled, dependency-free blocking HTTP/1.1 client carries the in-cluster RPC
(no TLS stack, matching the project's minimal-dependency ethos — the same reason
the Prometheus exposition is hand-rolled).

## Alternatives considered

- **Shared/external multi-writer store** (replace embedded SparrowDB; move assets
  to `ReadWriteMany`/object storage). Rejected for now: a large change to the
  storage substrate and the ADR-0001 dialect, and it would not, by itself, carry
  identity safely to a replica — the capability model would still be needed.
- **Share the embedded store across pods** (e.g. `ReadWriteMany` on the same
  files). Rejected: SparrowDB is single-writer; concurrent writers are undefined
  behaviour and two volumes diverge silently — a data-leak risk on the
  clearance-enforcement boundary.
- **Trust the executor with the `FirmContext` on callbacks** (no capability).
  Rejected: it makes every execution replica a forgery oracle for any identity —
  the exact invariant ADR-0002 exists to prevent.
- **One combined Service port vs. split ports.** The executor serves
  `/internal/invoke` and `/metrics` on one port (L3/L4 NetworkPolicy cannot split
  by path); the transport secret gates `/internal/invoke` and `/metrics` carries
  no secrets, so a single port is safe and simpler.

## Consequences

- Horizontal scale-out is unblocked for the execution tier; the #65 single-pod
  guardrail is retired (`scale-out-prerequisites.md` → `scale-out.md`), and the
  inert KEDA example is promoted to a live ScaledObject targeting the executor.
- The CP remains a single writer and a single point of failure for writes and for
  every executor callback; size it for the callback fan-in. (Highly-available
  control plane / leader election is a possible future ADR.)
- New operational surface: the `internal-token` Secret, a second Deployment +
  Service, two NetworkPolicies, a second ServiceMonitor, and the KEDA ScaledObject
  (`deploy/k8s/`). NetworkPolicy enforcement depends on the cluster CNI; the
  transport secret is the app-layer guard regardless.
- In-cluster CP↔executor transport is plain HTTP; in-cluster TLS (mesh/mTLS) is
  left to the deployment environment, consistent with the public API's transport
  note (`docs/SECURITY.md`).
- The split is verified end-to-end (`kanbrick-api/tests/executor.rs`): a real
  CP+executor loop with clearance-filtered callbacks, capability forgery rejected,
  and registry reconcile.
