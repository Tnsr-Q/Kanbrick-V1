#!/usr/bin/env bash
# "One Command" build — compile the entire Kanbrick-V1 workspace.
# Vendored upstreams (crates/*) are excluded from the workspace; ensure their
# submodule contents are present for later-phase integration.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> ensuring submodules are initialized"
git submodule update --init --depth 1 || true

echo "==> building workspace"
cargo build --workspace --all-features

echo "==> running tests"
cargo test --workspace

echo "==> done"
