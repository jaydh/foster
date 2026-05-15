#!/usr/bin/env bash
set -euo pipefail

# Build WASM (dev) and start one or all Foster demo servers.
# Usage:
#   ./scripts/demo.sh            — start all demos
#   ./scripts/demo.sh kanban     — start only kanban

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."

if [[ $# -eq 0 ]]; then
    DEMOS=(counter player kanban aura checkout plane notion)
else
    DEMOS=("$@")
fi

port_for() {
    case "$1" in
        counter)  echo 3000 ;;
        player)   echo 3001 ;;
        kanban)   echo 3002 ;;
        aura)     echo 3003 ;;
        checkout) echo 3004 ;;
        plane)    echo 3005 ;;
        notion)   echo 3006 ;;
        *) echo "Unknown demo: $1 (choose: counter, player, kanban, aura, checkout, plane, notion)" >&2; exit 1 ;;
    esac
}

# ── build WASM ────────────────────────────────────────────────────────────────
echo "▸ building foster-client (wasm-pack --dev)…"
cd crates/foster-client
wasm-pack build --dev --target web --out-dir pkg 2>&1 | grep -v "^\[WARN\]" || true
rm -rf ../../pkg
mv pkg ../../pkg
cd ../..

# ── start servers ─────────────────────────────────────────────────────────────
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
