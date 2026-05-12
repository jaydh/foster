#!/usr/bin/env bash
set -euo pipefail

# Start one or all Foster demo servers.
# Usage:
#   ./scripts/demo.sh            — start all four demos
#   ./scripts/demo.sh counter    — start only the counter demo
#   ./scripts/demo.sh player     — start only the player demo
#   ./scripts/demo.sh kanban     — start only the kanban demo
#   ./scripts/demo.sh aura       — start only the aura demo
#
# Each server runs in the background; Ctrl-C kills them all.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$SCRIPT_DIR/.."

DEMOS=("${@:-counter player kanban aura}")
# Flatten the array when no args are given (bash expands the string as one element)
if [[ $# -eq 0 ]]; then
    DEMOS=(counter player kanban aura)
fi

PIDS=()

cleanup() {
    echo ""
    echo "▸ stopping demos…"
    for pid in "${PIDS[@]:-}"; do
        kill "$pid" 2>/dev/null || true
    done
    exit 0
}
trap cleanup INT TERM

declare -A PORTS=(
    [counter]=3000
    [player]=3001
    [kanban]=3002
    [aura]=3003
)

for demo in "${DEMOS[@]}"; do
    port="${PORTS[$demo]:-}"
    if [[ -z "$port" ]]; then
        echo "Unknown demo: $demo (choose: counter, player, kanban, aura)"
        exit 1
    fi
    echo "▸ starting $demo  →  http://localhost:$port"
    cargo run -q -p "$demo" --bin "$demo" &
    PIDS+=($!)
done

echo ""
echo "All demos running. Press Ctrl-C to stop."
echo ""

for demo in "${DEMOS[@]}"; do
    echo "  http://localhost:${PORTS[$demo]}   ($demo)"
done
echo ""

wait
