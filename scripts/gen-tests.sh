#!/usr/bin/env bash
set -euo pipefail

# Re-generate Playwright test specs for all examples (or a specific one).
# Usage:
#   ./scripts/gen-tests.sh             — regenerate all
#   ./scripts/gen-tests.sh kanban      — regenerate only kanban

DEMOS=("${@:-counter player kanban aura}")
if [[ $# -eq 0 ]]; then
    DEMOS=(counter player kanban aura checkout plane notion)
fi

for demo in "${DEMOS[@]}"; do
    echo "▸ gen_tests  $demo"
    cargo run -q -p "$demo" --bin gen_tests
done

echo "✓ done"
