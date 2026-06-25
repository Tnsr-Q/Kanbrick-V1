#!/usr/bin/env bash
# Headless login -> /me smoke test (P7.6, #92).
#
# Drives the REAL kanbrick-api binary the same way the Cockpit's host commands do
# (spawn -> GET /health -> POST /login -> GET /me), with no webview or display, so
# it runs anywhere CI does. This locks the thin path's *contract* into CI: if the
# login/identity surface the Cockpit depends on ever regresses, this goes red.
#
# (A full Tauri-GUI driver e2e — tauri-driver + WebKitWebDriver under xvfb — is a
# possible future hardening; the contract smoke is the deterministic core.)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

EMAIL="brian.humphrey@kanbrick.com"   # exists in seed/kanbrick_seed_data.cypher
PASSWORD="cockpit-smoke-secret"
PORT="${SMOKE_PORT:-8771}"
BASE="http://127.0.0.1:${PORT}"

WORK="$(mktemp -d)"
DB="${WORK}/firm.db"
ASSETS="${WORK}/assets"
mkdir -p "$ASSETS"

API_PID=""
cleanup() {
  [ -n "$API_PID" ] && kill "$API_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

echo "==> Building kanbrick-api + kanbrick-cli (release)…"
cargo build --release -p kanbrick-api -p kanbrick-cli
API="${REPO_ROOT}/target/release/kanbrick-api"
CLI="${REPO_ROOT}/target/release/kanbrick-cli"

echo "==> Seeding firm data + setting a password for ${EMAIL}…"
"$CLI" seed --db "$DB" --file seed/kanbrick_seed_data.cypher
"$CLI" set-password --db "$DB" --email "$EMAIL" --password "$PASSWORD"

echo "==> Starting kanbrick-api on :${PORT}…"
"$API" --port "$PORT" --db "$DB" --asset-dir "$ASSETS" &
API_PID=$!

echo "==> Waiting for GET /health…"
for _ in $(seq 1 80); do
  curl -fsS "${BASE}/health" >/dev/null 2>&1 && break
  sleep 0.25
done
curl -fsS "${BASE}/health" >/dev/null || { echo "FAIL: /health never came up"; exit 1; }

echo "==> POST /login…"
TOKEN="$(curl -fsS -X POST "${BASE}/login" \
  -H 'content-type: application/json' \
  -d "{\"email\":\"${EMAIL}\",\"password\":\"${PASSWORD}\"}" | jq -r '.token')"
[ -n "$TOKEN" ] && [ "$TOKEN" != "null" ] || { echo "FAIL: no token from /login"; exit 1; }

echo "==> GET /me (Bearer)…"
ME="$(curl -fsS "${BASE}/me" -H "authorization: Bearer ${TOKEN}")"
echo "    /me -> ${ME}"
echo "$ME" | jq -e --arg e "$EMAIL" '.email == $e' >/dev/null \
  || { echo "FAIL: /me email did not match ${EMAIL}"; exit 1; }
echo "$ME" | jq -e '.clearance | test("^L[1-5]$")' >/dev/null \
  || { echo "FAIL: /me clearance is not L1..L5"; exit 1; }

echo "==> GET /me without a token must be 401…"
CODE="$(curl -s -o /dev/null -w '%{http_code}' "${BASE}/me")"
[ "$CODE" = "401" ] || { echo "FAIL: expected 401 unauthenticated, got ${CODE}"; exit 1; }

echo "SMOKE OK: login -> /me round-trip verified (+ unauthenticated 401)"
