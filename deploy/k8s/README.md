# Kubernetes deploy assets (#65, Track B)

Production-safe, **single-pod** manifests for the Kanbrick-V1 API, plus an inert
autoscaling template. These wrap the image built by [`../../Dockerfile`](../../Dockerfile).

| File | Kind(s) | Applied by default? | Validates with `--dry-run=client`? |
| --- | --- | --- | --- |
| `kanbrick-api.yaml` | PersistentVolumeClaim, Deployment, Service | ✅ yes | ✅ yes (core resources) |
| `servicemonitor.yaml` | ServiceMonitor (Prometheus Operator CRD) | optional | ❌ CRD — use kubeconform / `--dry-run=server` |
| `keda-scaledobject.example.yaml` | ScaledObject (KEDA CRD) | ❌ **no — inert example** | ❌ CRD — use kubeconform / `--dry-run=server` |
| `scale-out-prerequisites.md` | docs | — | — |

## Apply

```sh
# 1. JWT signing secret (required; the app falls back to an insecure dev secret).
kubectl create secret generic kanbrick-secrets \
  --from-literal=jwt-secret="$(openssl rand -hex 32)"

# 2. Core resources.
kubectl apply -f deploy/k8s/kanbrick-api.yaml

# 3. One-time seed (the image bundles the seed data + CLI).
kubectl exec deploy/kanbrick-api -- kanbrick-cli seed \
  --file /opt/kanbrick/seed/kanbrick_seed_data.cypher \
  --db /var/lib/kanbrick/firm.db

# 4. (Optional) Prometheus scraping, if you run the Prometheus Operator.
kubectl apply -f deploy/k8s/servicemonitor.yaml
```

The `keda-scaledobject.example.yaml` is **not** applied — see below.

## Storage & single-pod model

The graph DB (`firm.db`) and the content-addressed asset registry (`assets/`)
both live on one `ReadWriteOnce` PVC mounted at `/var/lib/kanbrick`, owned by a
single pod (`replicas: 1`, `strategy: Recreate`). This is a hard correctness
guardrail — see [`scale-out-prerequisites.md`](./scale-out-prerequisites.md)
before raising replica counts.

## Metrics exposure

`/metrics` is unauthenticated and its `guest="…"` labels reveal the guest
catalogue (`docs/SECURITY.md`). The `Service` is `ClusterIP` and the
`ServiceMonitor` scrapes it **in-cluster only** — do not route `/metrics` through
a public ingress.

## Autoscaling (deliberately inert)

`keda-scaledobject.example.yaml` is a template, pinned to a single replica
(`min == max == 1`). Horizontal scale-out is blocked by the single-writer store;
the KEDA wiring (Prometheus scaler over `kanbrick_mesh_pressure_ratio`, #63) is
provided so it is ready to enable once the prerequisites are met. There is no
GPU/DCGM trigger — guests run CPU-only in wasmtime.

## Validation

```sh
scripts/validate-k8s.sh
```

Checks YAML well-formedness always; runs [kubeconform](https://github.com/yannh/kubeconform)
(schema-aware, CRDs skipped via `-ignore-missing-schemas`) and a `kubectl`
client-side dry-run of the core manifest when those tools are present. CI runs
this on changes under `deploy/k8s/**` (`.github/workflows/k8s.yml`). Validating
the CRD resources fully requires a cluster with the Prometheus Operator + KEDA
installed (`--dry-run=server`), which CI skips.
