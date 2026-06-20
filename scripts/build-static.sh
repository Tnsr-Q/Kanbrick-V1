#!/usr/bin/env bash
# #53 deployment: build the fully-static (musl) Linux binary, verify it is
# statically linked and within the 100 MB budget, then smoke-test it
# (seed → serve → /health → login → guest query). The API embeds the three WASM
# guests via its build.rs, so the static binary is self-contained.
#
# Prerequisites on the build host:
#   * musl C toolchain:  apt-get install -y musl-tools   (provides musl-gcc)
#   * rustup target:     rustup target add x86_64-unknown-linux-musl  (auto-added below)
set -euo pipefail
cd "$(dirname "$0")/.."

TARGET="${TARGET:-x86_64-unknown-linux-musl}"
PORT="${PORT:-18081}"
OUT_DIR="target/${TARGET}/release"
API_BIN="${OUT_DIR}/kanbrick-api"
CLI_BIN="${OUT_DIR}/kanbrick-cli"

echo "==> ensuring the sparrowdb submodule is present (required to build)"
git submodule update --init --depth 1 crates/sparrowdb >/dev/null 2>&1 || true

if ! command -v musl-gcc >/dev/null 2>&1; then
  echo "::error::musl-gcc not found — install it first (apt-get install -y musl-tools)"
  exit 1
fi

echo "==> ensuring the ${TARGET} rust target is installed"
rustup target add "${TARGET}" >/dev/null 2>&1 || true

echo "==> building static binaries for ${TARGET} (release)"
SECONDS=0
cargo build --release --target "${TARGET}" --bin kanbrick-api --bin kanbrick-cli
echo "    build took ${SECONDS}s (target < 300s)"

echo "==> verifying the binary is statically linked"
file "${API_BIN}"
if ldd "${API_BIN}" 2>&1 | grep -qiE "statically linked|not a dynamic executable"; then
  echo "    OK: statically linked"
else
  echo "::error::kanbrick-api is not statically linked"
  ldd "${API_BIN}" || true
  exit 1
fi

echo "==> checking binary size (< 100 MB)"
size=$(wc -c <"${API_BIN}")
echo "    kanbrick-api = $((size / 1024 / 1024)) MB"
if ((size > 100 * 1024 * 1024)); then
  echo "::error::kanbrick-api exceeds the 100 MB budget"
  exit 1
fi

echo "==> smoke test: seed → serve → /health → login → reporting guest"
WORK="$(mktemp -d)"
DB="${WORK}/firm.db"
API_PID=""
cleanup() {
  [[ -n "${API_PID}" ]] && kill "${API_PID}" 2>/dev/null || true
  rm -rf "${WORK}"
}
trap cleanup EXIT

"${CLI_BIN}" seed --db "${DB}" >/dev/null
"${CLI_BIN}" set-password --email tracy.brittcool@kanbrick.com --password smoke-pw --db "${DB}"
KANBRICK_JWT_SECRET=smoke-secret "${API_BIN}" --port "${PORT}" --db "${DB}" >"${WORK}/api.log" 2>&1 &
API_PID=$!

healthy=""
for _ in $(seq 1 60); do
  if body=$(curl -fsS "http://127.0.0.1:${PORT}/health" 2>/dev/null); then
    healthy="$body"
    break
  fi
  sleep 0.5
done
if [[ -z "${healthy}" ]]; then
  echo "::error::API did not become healthy"
  cat "${WORK}/api.log"
  exit 1
fi
echo "    health: ${healthy}"

token=$(curl -fsS -X POST "http://127.0.0.1:${PORT}/login" \
  -H 'content-type: application/json' \
  -d '{"email":"tracy.brittcool@kanbrick.com","password":"smoke-pw"}' |
  sed -n 's/.*"token":"\([^"]*\)".*/\1/p')
[[ -n "${token}" ]] || { echo "::error::login failed"; exit 1; }

companies=$(curl -fsS -X POST "http://127.0.0.1:${PORT}/guests/reporting" \
  -H "Authorization: Bearer ${token}" \
  -H 'content-type: application/json' \
  -d '{}' | grep -o '"company_id"' | wc -l | tr -d ' ')
echo "    reporting dashboard returned ${companies} companies"
[[ "${companies}" == "9" ]] || { echo "::error::expected 9 companies, got ${companies}"; exit 1; }

echo "==> STATIC BUILD OK — ${API_BIN} ($((size / 1024 / 1024)) MB, static, smoke-passed)"
