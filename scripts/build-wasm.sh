#!/usr/bin/env bash
set -euo pipefail

# Build WASM client and place pkg/ at workspace root.
# Run this once (or after any change to crates/foster-client).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$SCRIPT_DIR/.."

echo "▸ Building foster-client (wasm-pack)…"
cd "$REPO/crates/foster-client"
wasm-pack build --target web --out-dir pkg

echo "▸ Moving pkg/ to workspace root…"
rm -rf "$REPO/pkg"
mv pkg "$REPO/pkg"

echo "✓ WASM built → $REPO/pkg"
