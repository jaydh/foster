#!/usr/bin/env bash
set -euo pipefail

# Fast LLM iteration loop — run after every change.
#
# Steps:
#   1. cargo check  (fast type check, no codegen)
#   2. cargo test   (unit tests — foster-core + foster-testgen)
#   3. gen_tests    (regenerate Playwright specs + print coverage summary)
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
T0=$SECONDS

# ── cargo check ───────────────────────────────────────────────────────────────
printf "▸ cargo check…  "
cargo check --workspace 2>&1
echo "(${SECONDS}s)"

# ── cargo test ────────────────────────────────────────────────────────────────
T1=$SECONDS
printf "▸ cargo test…   "
cargo test --workspace --quiet 2>&1
echo "($((SECONDS - T1))s)"

# ── gen_tests ─────────────────────────────────────────────────────────────────
T2=$SECONDS
echo "▸ gen_tests"
if [[ -n "$TARGET" ]]; then
    cargo run -q -p "$TARGET" --bin gen_tests 2>&1
else
    bash scripts/gen-tests.sh 2>&1
fi
echo "  ($((SECONDS - T2))s)"

echo ""
echo "✓  all checks passed  (total: $((SECONDS - T0))s)"
