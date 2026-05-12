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
cd "$SCRIPT_DIR/.."

if [[ $# -eq 0 ]]; then
    DEMOS=(counter player kanban aura)
else
    DEMOS=("$@")
fi

port_for() {
    case "$1" in
        counter) echo 3000 ;;
        player)  echo 3001 ;;
        kanban)  echo 3002 ;;
        aura)    echo 3003 ;;
        *) echo "Unknown demo: $1 (choose: counter, player, kanban, aura)" >&2; exit 1 ;;
    esac
}

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

for demo in "${DEMOS[@]}"; do
    port="$(port_for "$demo")"
    echo "▸ starting $demo  →  http://localhost:$port"
    cargo run -q -p "$demo" --bin "$demo" &
    PIDS+=($!)
done

echo ""
echo "All demos running. Press Ctrl-C to stop."
echo ""
for demo in "${DEMOS[@]}"; do
    echo "  http://localhost:$(port_for "$demo")   ($demo)"
done
echo ""

wait
