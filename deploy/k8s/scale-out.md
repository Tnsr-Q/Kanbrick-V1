# Horizontal scale-out: the control-plane / executor split

Kanbrick-V1 scales out as **two tiers** (the split landed across #68–#71; see
[ADR-0008](../../docs/adr/0008-control-plane-executor-split.md)):

- a **control plane** (`kanbrick-api.yaml`) — a single pod that owns the embedded
  SparrowDB graph, the content-addressed asset registry, every write, and the
  authoritative identity. It stays `replicas: 1`, `strategy: Recreate`.
- a **stateless executor pool** (`kanbrick-executor.yaml`) — a horizontally
  scalable tier that runs guest WASM. It holds **no** single-writer state, so it
  is the tier KEDA scales (`keda-scaledobject.yaml`).

This **retires the single-pod guardrail** that previously blocked scale-out (the
old `scale-out-prerequisites.md`): the embedded-store / local-asset blockers no
longer gate replicas, because the only tier that scales — the executor — touches
neither.

## How it works now

1. A request hits the control plane: **auth → clearance → admission → audit** all
   stay on the CP.
2. The CP mints a short-lived, single-invocation **capability** bound to the
   caller's host-authoritative `FirmContext`, and forwards the invocation to the
   executor pool (`KANBRICK_EXECUTOR_URL` → the executor `Service`), relaying only
   the opaque capability — never the identity.
3. An executor runs the guest. When the guest reads the graph or emits an event,
   the executor proxies that call **back** to the CP's internal RPC surface
   (`KANBRICK_CP_URL` → `kanbrick-api:8090`), presenting the capability. The CP
   resolves the capability to the bound identity **server-side** and runs the read
   through the clearance-enforcing `GuardedStore`. A forged or expired capability
   is a `401` ⇒ the guest's query traps (no data leak).
4. The CP revokes the capability the moment the invocation returns.

The executor reconciles the activated-guest set from the CP on boot and on a
registry-generation bump (after an L5 `activate_guest`), pulling assets and
hot-reloading — so the pool stays consistent without shared storage.

Back-compat: with **no** `KANBRICK_EXECUTOR_URL`, the control plane runs guests
in-process exactly as the prior single-pod deployment did.

## What scales, and on what signal

KEDA scales **only the executor** Deployment on `kanbrick_mesh_pressure_ratio`
(#63) — the in-flight guest concurrency over capacity, aggregated across executor
pods (`max(...)`) from their `/metrics`. The control plane is never scaled
(`minReplicaCount`/`maxReplicaCount` apply to the executor; the CP has no
ScaledObject). There is no GPU/DCGM trigger — guests run CPU-only in wasmtime.

## Network confinement

`networkpolicy.yaml` pins the topology:

- the CP's internal RPC port (`8090`) is reachable **only** by executor pods;
- the executor is reachable **only** by the control plane (`/internal/invoke`) and
  Prometheus (`/metrics`) — it has **no** public ingress.

The shared transport secret (`x-kanbrick-internal-token`, the
`internal-token` Secret key) gates both internal surfaces at the application layer,
independent of whether the cluster's CNI enforces NetworkPolicy.

## Capacity notes

- The CP is a single writer: its throughput bounds total **write** and
  **activation** rate, and it services every executor graph/event callback. Size
  its CPU/memory for the callback fan-in, not for guest execution.
- Executors are disposable and CPU-bound; tune `maxReplicaCount`, per-pod
  `resources`, and per-guest concurrency (`KANBRICK_GUEST_CONCURRENCY`) to your
  cluster.
