# Horizontal scale-out prerequisites

The Kanbrick-V1 API ships **single-pod only**. The base `Deployment` pins
`replicas: 1` and the KEDA example pins `maxReplicaCount: 1`. This is a
correctness guardrail, not a tuning default — running two API pods today would
corrupt or lose data. This document explains why, and what must change first.

## Why scale-out is blocked

1. **SparrowDB is embedded and single-writer.** `kanbrick-store` opens a
   file-backed `GraphDb` at `--db` (`/var/lib/kanbrick/firm.db`) under a
   single-writer / multiple-reader model (`kanbrick-store/src/store.rs`). It is
   the source of truth for business state *and* for guest policy
   (`(:GuestPolicy)` nodes, #64). Two pods writing the same files concurrently is
   undefined behaviour; two pods on two volumes diverge silently — a data-leak
   risk on the clearance-enforcement boundary (ADR-0001 §Decision).

2. **The asset registry is a local volume.** Activated guest artifacts live on
   the content-addressed volume at `--asset-dir` (`/var/lib/kanbrick/assets`,
   #64). A second pod on a separate `ReadWriteOnce` volume would not see guests
   activated by the first.

Because both the graph and the assets share one `ReadWriteOnce` PVC owned by one
pod, the `Deployment` also uses `strategy: Recreate` — the volume cannot be held
by an old and a new pod at the same time during a rollout.

## What must change before raising replicas

Pick one of:

- **Shared/external graph store.** Replace the embedded SparrowDB with a
  network-accessible, multi-writer store (or a SparrowDB deployment that supports
  concurrent writers), and move the asset registry onto shared storage
  (`ReadWriteMany`, or an object store such as S3/MinIO addressed by the same
  `tachyon://sha256:<hex>` content hash). Then every replica reads/writes one
  source of truth.

- **API/worker split.** Keep a single writer: route all graph **writes** and all
  guest **activations** to one writer pod (or a leader), and scale only
  stateless **read/execute** replicas behind it. Requires the write path to be
  cleanly separable and guest side effects to be idempotent/re-routable.

Either path is a design change beyond the scope of the deploy manifests and is
intentionally **not** attempted here.

## When unblocked

1. Implement one of the options above.
2. Raise `replicas` in `kanbrick-api.yaml` (and switch `strategy` away from
   `Recreate` once the volume is no longer single-attach).
3. Enable autoscaling: drop the `.example` suffix from
   `keda-scaledobject.example.yaml`, raise `maxReplicaCount`, point
   `serverAddress` at your Prometheus, and apply it. The scale signal,
   `kanbrick_mesh_pressure_ratio` (#63), already aggregates mesh pressure across
   the fleet via `max(...)`.
