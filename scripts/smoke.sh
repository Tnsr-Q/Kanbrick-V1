#!/usr/bin/env bash
# #51 "One Command" validation: build the workspace in release, seed a fresh DB,
# start the API, hit /health, run a login-and-query cycle, and shut down — the
# clone-to-running-system smoke test. Budgets: release build < 5 min, binary
# < 100 MB.
set -euo pipefail
cd "$(dirname "$0")/.."

PORT="${PORT:-18080}"
WORK="$(mktemp -d)"
DB="$WORK/firm.db"
API_PID=""
cleanup() {
  [[ -n "$API_PID" ]] && kill "$API_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

echo "==> ensuring the sparrowdb submodule is present"
git submodule update --init --depth 1 crates/sparrowdb >/dev/null 2>&1 || true

echo "==> building the workspace (release)"
SECONDS=0
cargo build --release --workspace
build_secs=$SECONDS
echo "    release build took ${build_secs}s (target < 300s)"

API_BIN=target/release/kanbrick-api
CLI_BIN=target/release/kanbrick-cli

echo "==> checking binary size (< 100 MB)"
size=$(wc -c <"$API_BIN")
echo "    kanbrick-api = $((size / 1024 / 1024)) MB"
if ((size > 100 * 1024 * 1024)); then
  echo "::error::kanbrick-api exceeds the 100 MB budget"
  exit 1
fi

echo "==> seeding the database"
"$CLI_BIN" seed --db "$DB"
"$CLI_BIN" set-password --email tracy.brittcool@kanbrick.com --password smoke-pw --db "$DB"

echo "==> starting the API on :$PORT"
KANBRICK_JWT_SECRET=smoke-secret "$API_BIN" --port "$PORT" --db "$DB" >"$WORK/api.log" 2>&1 &
API_PID=$!

echo "==> waiting for /health"
healthy=""
for _ in $(seq 1 60); do
  if body=$(curl -fsS "http://127.0.0.1:$PORT/health" 2>/dev/null); then
    healthy="$body"
    break
  fi
  sleep 0.5
done
if [[ -z "$healthy" ]]; then
  echo "::error::API did not become healthy"
  cat "$WORK/api.log"
  exit 1
fi
echo "    health: $healthy"
echo "$healthy" | grep -q '"status":"healthy"' || { echo "::error::unhealthy"; exit 1; }

echo "==> login + guest-query cycle"
token=$(curl -fsS -X POST "http://127.0.0.1:$PORT/login" \
  -H 'content-type: application/json' \
  -d '{"email":"tracy.brittcool@kanbrick.com","password":"smoke-pw"}' |
  sed -n 's/.*"token":"\([^"]*\)".*/\1/p')
[[ -n "$token" ]] || { echo "::error::login failed"; exit 1; }

dashboard=$(curl -fsS -X POST "http://127.0.0.1:$PORT/guests/reporting" \
  -H "Authorization: Bearer $token" \
  -H 'content-type: application/json' \
  -d '{}')
companies=$(echo "$dashboard" | grep -o '"company_id"' | wc -l | tr -d ' ')
echo "    reporting dashboard returned ${companies} companies"
[[ "$companies" == "9" ]] || { echo "::error::expected 9 companies, got $companies"; exit 1; }

echo "==> ONE-COMMAND SMOKE TEST PASSED (build ${build_secs}s, binary $((size / 1024 / 1024)) MB)"
