# Kubernetes deploy assets (#65 Track B, #71 Track G)

Two-tier manifests for Kanbrick-V1: a single-pod **control plane** (owns the
graph, asset registry, and every write) and a stateless, autoscaled **executor
pool** (runs guest WASM). Both wrap the image built by
[`../../Dockerfile`](../../Dockerfile) — the run-mode is selected with
`--mode`. See [`scale-out.md`](./scale-out.md) and
[ADR-0008](../../docs/adr/0008-control-plane-executor-split.md) for how the split
works.

| File | Kind(s) | Applied by default? | Validates with `--dry-run=client`? |
| --- | --- | --- | --- |
| `kanbrick-api.yaml` | PersistentVolumeClaim, Deployment, Service | ✅ yes (control plane) | ✅ yes (core resources) |
| `kanbrick-executor.yaml` | Deployment, Service | ✅ yes (executor pool) | ✅ yes (core resources) |
| `networkpolicy.yaml` | NetworkPolicy ×2 | ✅ yes | ✅ yes (core resources) |
| `servicemonitor.yaml` | ServiceMonitor (Prometheus Operator CRD) | optional | ❌ CRD — kubeconform / `--dry-run=server` |
| `servicemonitor-executor.yaml` | ServiceMonitor (CRD) | optional | ❌ CRD — kubeconform / `--dry-run=server` |
| `keda-scaledobject.yaml` | ScaledObject (KEDA CRD) | optional (needs KEDA) | ❌ CRD — kubeconform / `--dry-run=server` |
| `scale-out.md` | docs | — | — |

## Apply

```sh
# 1. Secrets: JWT signing secret + the shared internal transport secret. The same
#    internal-token value is read by both the control plane and the executors.
kubectl create secret generic kanbrick-secrets \
  --from-literal=jwt-secret="$(openssl rand -hex 32)" \
  --from-literal=internal-token="$(openssl rand -hex 32)"

# 2. Control plane (single-writer: graph + assets + internal RPC surface).
kubectl apply -f deploy/k8s/kanbrick-api.yaml

# 3. One-time seed (the image bundles the seed data + CLI).
kubectl exec deploy/kanbrick-api -- kanbrick-cli seed \
  --file /opt/kanbrick/seed/kanbrick_seed_data.cypher \
  --db /var/lib/kanbrick/firm.db

# 4. Executor pool (stateless; boots against the control plane).
kubectl apply -f deploy/k8s/kanbrick-executor.yaml

# 5. Network confinement (internal port → executors only; executor not public).
kubectl apply -f deploy/k8s/networkpolicy.yaml

# 6. (Optional) Prometheus scraping, if you run the Prometheus Operator.
kubectl apply -f deploy/k8s/servicemonitor.yaml
kubectl apply -f deploy/k8s/servicemonitor-executor.yaml

# 7. (Optional) Executor autoscaling, if you run KEDA.
kubectl apply -f deploy/k8s/keda-scaledobject.yaml
```

## Topology & storage

The control plane is a **single writer**: the graph DB (`firm.db`) and the
content-addressed asset registry (`assets/`) both live on one `ReadWriteOnce` PVC
mounted at `/var/lib/kanbrick`, owned by one pod (`replicas: 1`,
`strategy: Recreate`). It is **not** scaled.

The executor pool is **stateless** — no PVC, no graph, no JWT, no public surface.
It runs guest WASM and proxies graph/event callbacks back to the control plane's
internal RPC surface under a per-invocation capability (graph reads stay
clearance-enforced on the CP; identity is never trusted from the wire). Because it
holds no single-writer state, it is the tier that scales horizontally.

## Network confinement

`networkpolicy.yaml` confines the internal RPC port (`8090`) to executor pods and
keeps the executor off any public ingress (only the control plane and Prometheus
may reach it). The shared transport secret gates both internal surfaces at the
application layer regardless of CNI enforcement. See
[`docs/SECURITY.md`](../../docs/SECURITY.md).

## Metrics exposure

Both tiers serve `/metrics` (unauthenticated; the `guest="…"` labels reveal the
guest catalogue — see `docs/SECURITY.md`). The `Service`s are `ClusterIP` and the
`ServiceMonitor`s scrape them **in-cluster only** — never route `/metrics` through
a public ingress.

## Autoscaling

`keda-scaledobject.yaml` scales the **executor** Deployment on
`kanbrick_mesh_pressure_ratio` (#63), `minReplicaCount: 1` to
`maxReplicaCount: 10` (tune to your cluster). The control plane is never scaled.
There is no GPU/DCGM trigger — guests run CPU-only in wasmtime. Requires the KEDA
CRDs (keda.sh); point `serverAddress` at your Prometheus and adjust the metric's
label selector to match your scrape relabeling.

## Validation

```sh
scripts/validate-k8s.sh
```

Checks YAML well-formedness always; runs [kubeconform](https://github.com/yannh/kubeconform)
(schema-aware, CRDs skipped via `-ignore-missing-schemas`) and a `kubectl`
client-side dry-run of the core manifest when those tools are present. CI runs
this on changes under `deploy/k8s/**` (`.github/workflows/k8s.yml`). Fully
validating the CRD resources (ServiceMonitor, ScaledObject) requires a cluster
with the Prometheus Operator + KEDA installed (`--dry-run=server`), which CI skips.
