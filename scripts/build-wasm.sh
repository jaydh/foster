#!/usr/bin/env bash
set -euo pipefail

# Build WASM client and place pkg/ at workspace root.
# Usage:
#   ./scripts/build-wasm.sh        — release build
#   ./scripts/build-wasm.sh --dev  — debug build (enables dev overlay)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$SCRIPT_DIR/.."

PROFILE_FLAG=""
if [[ "${1:-}" == "--dev" ]]; then
    PROFILE_FLAG="--dev"
fi

echo "▸ Building foster-client (wasm-pack${PROFILE_FLAG:+ $PROFILE_FLAG})…"
cd "$REPO/crates/foster-client"
# shellcheck disable=SC2086
wasm-pack build $PROFILE_FLAG --target web --out-dir pkg

echo "▸ Moving pkg/ to workspace root…"
rm -rf "$REPO/pkg"
mv pkg "$REPO/pkg"

echo "✓ WASM built → $REPO/pkg"
