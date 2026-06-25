#!/usr/bin/env bash
# Build kanbrick-api for the host triple and stage it as the Tauri sidecar.
#
# Tauri's `externalBin` expects `cockpit/src-tauri/binaries/kanbrick-api-<triple>`.
# This runs automatically via `beforeDevCommand`/`beforeBuildCommand`; it is also
# safe to run by hand (`npm run sidecar`). `cargo build` is incremental, so repeat
# runs are cheap.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEST_DIR="$REPO_ROOT/cockpit/src-tauri/binaries"

# Host target triple, e.g. x86_64-unknown-linux-gnu.
TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
if [ -z "$TRIPLE" ]; then
  echo "prepare-sidecar: could not determine host target triple from rustc" >&2
  exit 1
fi

echo "prepare-sidecar: building kanbrick-api (release) for $TRIPLE…"
cargo build --release -p kanbrick-api --manifest-path "$REPO_ROOT/Cargo.toml"

EXT=""
case "$TRIPLE" in
  *windows*) EXT=".exe" ;;
esac
SRC="$REPO_ROOT/target/release/kanbrick-api$EXT"

if [ ! -f "$SRC" ]; then
  echo "prepare-sidecar: expected binary not found at $SRC" >&2
  exit 1
fi

mkdir -p "$DEST_DIR"
cp "$SRC" "$DEST_DIR/kanbrick-api-$TRIPLE$EXT"
echo "prepare-sidecar: staged → $DEST_DIR/kanbrick-api-$TRIPLE$EXT"
