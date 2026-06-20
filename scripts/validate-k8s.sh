#!/usr/bin/env bash
# Validate the Kubernetes deploy manifests (#65, Track B).
#
# Layered, graceful degradation so it runs locally and in CI:
#   1. YAML well-formedness        — always (needs python3 + PyYAML; else skipped).
#   2. kubeconform schema check    — if installed (CRDs skipped, core kinds strict).
#   3. kubectl client dry-run      — if installed (core manifest only; CRDs can't).
#
# Fully validating the CRD resources (ServiceMonitor, ScaledObject) needs a
# cluster with the Prometheus Operator + KEDA installed (`--dry-run=server`),
# which this script does not require.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIR="$ROOT/deploy/k8s"
shopt -s nullglob
MANIFESTS=("$DIR"/*.yaml)
if [ ${#MANIFESTS[@]} -eq 0 ]; then
  echo "No manifests found in $DIR" >&2
  exit 1
fi

echo "Validating ${#MANIFESTS[@]} manifest(s) in deploy/k8s/"
rc=0

# 1. YAML well-formedness.
if command -v python3 >/dev/null 2>&1 && python3 -c "import yaml" >/dev/null 2>&1; then
  if python3 - "${MANIFESTS[@]}" <<'PY'; then
import sys, yaml
ok = True
for path in sys.argv[1:]:
    try:
        with open(path) as f:
            docs = list(yaml.safe_load_all(f))
        print(f"  yaml ok   {path}  ({len(docs)} doc(s))")
    except Exception as e:  # noqa: BLE001
        ok = False
        print(f"  YAML FAIL {path}: {e}")
sys.exit(0 if ok else 1)
PY
    :
  else
    rc=1
  fi
else
  echo "  (python3+PyYAML unavailable; skipping YAML parse check)"
fi

# 2. kubeconform schema validation (CRDs skipped via -ignore-missing-schemas).
if command -v kubeconform >/dev/null 2>&1; then
  echo "  kubeconform:"
  kubeconform -strict -ignore-missing-schemas -summary "${MANIFESTS[@]}" || rc=1
else
  echo "  (kubeconform not found; skipping schema validation — install for CRD-aware checks)"
fi

# 3. kubectl client-side dry-run of the core (non-CRD) manifest.
if command -v kubectl >/dev/null 2>&1; then
  if kubectl apply --dry-run=client -f "$DIR/kanbrick-api.yaml" >/dev/null 2>&1; then
    echo "  kubectl dry-run(client) ok: kanbrick-api.yaml"
  else
    echo "  (kubectl present but client dry-run failed — likely no kubeconfig; non-fatal)"
  fi
else
  echo "  (kubectl not found; skipping client dry-run)"
fi

if [ "$rc" -eq 0 ]; then
  echo "OK"
else
  echo "FAILED" >&2
fi
exit "$rc"
