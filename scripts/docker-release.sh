#!/usr/bin/env bash
# #53 deployment: build the runtime container image, report its size (target
# < 150 MB), and smoke-test it (seed → serve → /health) before tagging a release.
set -euo pipefail
cd "$(dirname "$0")/.."

IMAGE="${IMAGE:-kanbrick-api}"
TAG="${1:-$(git rev-parse --short HEAD 2>/dev/null || echo dev)}"
PORT="${PORT:-18090}"

echo "==> ensuring the sparrowdb submodule is present (required to build)"
git submodule update --init --depth 1 crates/sparrowdb >/dev/null 2>&1 || true

echo "==> docker build ${IMAGE}:${TAG}"
docker build -t "${IMAGE}:${TAG}" -t "${IMAGE}:latest" .

size_mb=$(docker image inspect "${IMAGE}:${TAG}" --format '{{.Size}}' |
  awk '{printf "%d", $1 / 1024 / 1024}')
echo "==> image size = ${size_mb} MB (target < 150 MB)"
if ((size_mb > 150)); then
  echo "::warning::image is ${size_mb} MB, above the 150 MB target (slim it with a smaller base or a stripped binary)"
fi

echo "==> smoke test: seed + serve + /health inside the container"
cid=$(docker run -d -p "${PORT}:8080" --entrypoint sh "${IMAGE}:${TAG}" -c \
  "kanbrick-cli seed --file /opt/kanbrick/seed/kanbrick_seed_data.cypher --db /var/lib/kanbrick/firm.db \
   && kanbrick-api --port 8080 --db /var/lib/kanbrick/firm.db")
cleanup() { docker rm -f "$cid" >/dev/null 2>&1 || true; }
trap cleanup EXIT

ok=""
for _ in $(seq 1 60); do
  if curl -fsS "http://127.0.0.1:${PORT}/health" >/dev/null 2>&1; then
    ok=1
    break
  fi
  sleep 1
done
if [[ -z "$ok" ]]; then
  echo "::error::container did not become healthy"
  docker logs "$cid"
  exit 1
fi
echo "    healthy: $(curl -fsS "http://127.0.0.1:${PORT}/health")"
echo "==> DOCKER RELEASE OK — ${IMAGE}:${TAG} (${size_mb} MB)"
