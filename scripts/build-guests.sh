#!/usr/bin/env bash
# Build all WASM business guests to wasm32-wasip1 (release) and validate their
# size (issue #40). The artifacts land in target/wasm32-wasip1/release/.
set -euo pipefail
cd "$(dirname "$0")/.."

MAX_BYTES=$((10 * 1024 * 1024)) # 10 MiB per guest
GUESTS=(valuation reporting compliance)

echo "==> building WASM guests for wasm32-wasip1 (release)"
cargo build --release --target wasm32-wasip1 \
  -p kanbrick-guest-valuation \
  -p kanbrick-guest-reporting \
  -p kanbrick-guest-compliance

out="target/wasm32-wasip1/release"
fail=0
echo "==> verifying artifacts (< 10 MiB each)"
for g in "${GUESTS[@]}"; do
  wasm="$out/kanbrick_guest_${g}.wasm"
  if [[ ! -f "$wasm" ]]; then
    echo "::error::missing guest artifact: $wasm"
    fail=1
    continue
  fi
  size=$(wc -c <"$wasm")
  printf '    %-11s %9d bytes  (%s)\n' "$g" "$size" "$wasm"
  if ((size > MAX_BYTES)); then
    echo "::error::$g artifact is ${size} bytes, exceeding the 10 MiB budget"
    fail=1
  fi
done

if ((fail == 0)); then
  echo "==> all guests built to wasm32-wasip1 and within the size budget"
else
  echo "==> WASM guest build validation FAILED"
  exit 1
fi
