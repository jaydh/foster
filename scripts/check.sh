#!/usr/bin/env bash
set -euo pipefail

# Fast LLM iteration loop — run after every change.
#
# Steps:
#   1. cargo check  (fast type check, no codegen)
#   2. cargo test   (unit tests — foster-core + foster-testgen)
#   3. gen_tests    (regenerate Playwright specs from machine definitions)
#
# Playwright / browser tests are NOT run here — use `npx playwright test`
# inside an example directory after starting the server with demo.sh.
#
# Usage:
#   ./scripts/check.sh           — check whole workspace
#   ./scripts/check.sh counter   — check + gen_tests for one example

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."

TARGET="${1:-}"

echo "▸ cargo check…"
cargo check --workspace 2>&1

echo "▸ cargo test…"
cargo test --workspace 2>&1

if [[ -n "$TARGET" ]]; then
    echo "▸ gen_tests  $TARGET"
    cargo run -q -p "$TARGET" --bin gen_tests
else
    echo "▸ gen_tests  (all)"
    bash scripts/gen-tests.sh
fi

echo ""
echo "✓ all checks passed"
