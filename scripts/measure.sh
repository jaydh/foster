#!/usr/bin/env bash
set -euo pipefail

# Measure LLM surface area: Foster vs React for the same app specs.
#
# Metrics
# ───────
# LOC       Non-blank, non-comment application lines (excludes config/generated code)
# Tokens    Approximate token count (chars ÷ 4 — within 15% of tiktoken for code)
# Concepts  Number of distinct language/framework concepts an LLM must reason about
#           (counted by examining the implementation, not automated)
#
# Files measured
# ──────────────
# Foster : main.rs (machine + reducers + server setup)
#          index.html (template)
#          — excludes gen_tests.rs (boilerplate) and generated .spec.ts
#
# React  : App.tsx + types.ts (if present)
#          App.test.tsx
#          — excludes package.json, tsconfig.json, vite.config.ts

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$SCRIPT_DIR/.."

count_loc() {
    local file="$1"
    # Non-blank lines that aren't pure comments (// or # or <!-- or *)
    grep -cEv '^\s*(//|#|<!--|/\*|\*|$)' "$file" 2>/dev/null || echo 0
}

count_tokens() {
    local file="$1"
    local chars
    chars=$(wc -c < "$file")
    echo $(( chars / 4 ))
}

measure_set() {
    local label="$1"; shift
    local total_loc=0 total_tokens=0
    echo "  $label"
    for f in "$@"; do
        if [[ -f "$f" ]]; then
            local loc tokens
            loc=$(count_loc "$f")
            tokens=$(count_tokens "$f")
            total_loc=$(( total_loc + loc ))
            total_tokens=$(( total_tokens + tokens ))
            printf "    %-50s  %4d loc  ~%5d tokens\n" "$(basename "$f")" "$loc" "$tokens"
        fi
    done
    printf "    %-50s  %4d loc  ~%5d tokens\n" "TOTAL" "$total_loc" "$total_tokens"
    echo ""
    # Export for comparison
    LAST_LOC=$total_loc
    LAST_TOKENS=$total_tokens
}

# ─────────────────────────────────────────────────────────────────────────────

echo ""
echo "══════════════════════════════════════════════════════════════════"
echo "  Foster vs React — LLM surface area benchmark"
echo "  Token estimate: chars ÷ 4  (rough but consistent)"
echo "══════════════════════════════════════════════════════════════════"
echo ""

# ── Counter ───────────────────────────────────────────────────────────────────

echo "── Counter app ─────────────────────────────────────────────────────"
echo ""

measure_set "Foster" \
    "$REPO/examples/counter/src/main.rs" \
    "$REPO/examples/counter/static/index.html"
foster_counter_loc=$LAST_LOC
foster_counter_tok=$LAST_TOKENS

measure_set "React" \
    "$REPO/benchmarks/counter/react/src/App.tsx" \
    "$REPO/benchmarks/counter/react/src/App.test.tsx"
react_counter_loc=$LAST_LOC
react_counter_tok=$LAST_TOKENS

echo "  Δ Foster saves: $(( react_counter_loc - foster_counter_loc )) loc  ~$(( react_counter_tok - foster_counter_tok )) tokens vs React"
echo ""

# ── Kanban ────────────────────────────────────────────────────────────────────

echo "── Kanban app ──────────────────────────────────────────────────────"
echo ""

measure_set "Foster" \
    "$REPO/examples/kanban/src/main.rs" \
    "$REPO/examples/kanban/static/index.html"
foster_kanban_loc=$LAST_LOC
foster_kanban_tok=$LAST_TOKENS

measure_set "React" \
    "$REPO/benchmarks/kanban/react/src/App.tsx" \
    "$REPO/benchmarks/kanban/react/src/types.ts" \
    "$REPO/benchmarks/kanban/react/src/App.test.tsx"
react_kanban_loc=$LAST_LOC
react_kanban_tok=$LAST_TOKENS

echo "  Δ Foster saves: $(( react_kanban_loc - foster_kanban_loc )) loc  ~$(( react_kanban_tok - foster_kanban_tok )) tokens vs React"
echo ""

# ── Summary ───────────────────────────────────────────────────────────────────

echo "══════════════════════════════════════════════════════════════════"
echo "  Summary"
echo "══════════════════════════════════════════════════════════════════"
printf "  %-12s  %8s  %12s\n" "App" "LOC" "~Tokens"
printf "  %-12s  %8d  %12d\n" "counter/foster" "$foster_counter_loc" "$foster_counter_tok"
printf "  %-12s  %8d  %12d\n" "counter/react"  "$react_counter_loc"  "$react_counter_tok"
printf "  %-12s  %8d  %12d\n" "kanban/foster"  "$foster_kanban_loc"  "$foster_kanban_tok"
printf "  %-12s  %8d  %12d\n" "kanban/react"   "$react_kanban_loc"   "$react_kanban_tok"
echo ""
echo "  Note: token counts exclude:"
echo "    - config files (package.json, Cargo.toml, tsconfig.json)"
echo "    - generated code (*.spec.ts, gen_tests.rs)"
echo "    - boilerplate (vite.config.ts, playwright.config.ts)"
echo ""
echo "  The hidden Foster advantage not captured here:"
echo "    - Playwright tests are generated from the machine — 0 tokens to write"
echo "    - Adding a feature = 1 .on() call + 1 reducer + 1 HTML attribute"
echo "    - React tests require knowing component internals; Foster tests"
echo "      only know state names and event names (the same as the implementation)"
echo ""
