#!/usr/bin/env bash
# Type-check, test and lint the workspace (the native crates; the WASM
# client builds with scripts/build-wasm.sh). Browser smoke test:
#   ./scripts/build-wasm.sh && (cd examples/counter && npx playwright test)
set -euo pipefail
cd "$(dirname "$0")/.."
cargo test --workspace
cargo clippy --workspace --all-targets
