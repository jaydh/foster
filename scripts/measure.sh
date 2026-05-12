#!/usr/bin/env bash
set -euo pipefail

# Measure LLM surface area: Foster vs React for the same app specs.
#
# Apples-to-apples rules
# ──────────────────────
# 1. CSS is excluded from Foster's index.html (<style> blocks).
#    React implementations also have no styling — equal footing.
# 2. Server setup is excluded from Foster's main.rs (fn main body + tokio::main).
#    That's identical boilerplate across all Foster apps; React doesn't carry it.
# 3. "Implementation" and "tests" are measured separately.
#    Foster generates tests from the machine — test authoring cost is 0.
#    React tests are hand-written — their cost is real.
#
# Metrics
# ───────
# LOC    Non-blank, non-comment lines
# Tokens chars ÷ 4  (within ~15% of tiktoken for code; consistent across runs)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$SCRIPT_DIR/.."

# ── filters ───────────────────────────────────────────────────────────────────

# Strip <style>…</style> blocks from HTML, then count non-blank/non-comment lines.
loc_html() {
    awk '/<style[ >]/{skip=1} skip{if(/<\/style>/){skip=0} next} 1' "$1" \
        | grep -cEv '^\s*(<!--|-->|//|/\*|\*|$)' 2>/dev/null || echo 0
}

tok_html() {
    local chars
    chars=$(awk '/<style[ >]/{skip=1} skip{if(/<\/style>/){skip=0} next} 1' "$1" | wc -c)
    echo $(( chars / 4 ))
}

# Strip fn main() + #[tokio::main] and everything after from Rust files.
# Application logic (reducers + machine builder) precedes the server entry point.
loc_rust_app() {
    awk '/^#\[tokio::main\]|^async fn main\(\)/{exit} 1' "$1" \
        | grep -cEv '^\s*(//|/\*|\*|$)' 2>/dev/null || echo 0
}

tok_rust_app() {
    local chars
    chars=$(awk '/^#\[tokio::main\]|^async fn main\(\)/{exit} 1' "$1" | wc -c)
    echo $(( chars / 4 ))
}

# Plain LOC + tokens for any file (React, types, tests).
loc_plain() {
    grep -cEv '^\s*(//|/\*|\*|$|import )' "$1" 2>/dev/null || echo 0
}

tok_plain() {
    local chars; chars=$(wc -c < "$1"); echo $(( chars / 4 ))
}

# ── report helpers ────────────────────────────────────────────────────────────

TOTAL_LOC=0
TOTAL_TOK=0

row() {
    local label="$1" loc="$2" tok="$3"
    printf "    %-52s  %4d loc  ~%5d tokens\n" "$label" "$loc" "$tok"
    TOTAL_LOC=$(( TOTAL_LOC + loc ))
    TOTAL_TOK=$(( TOTAL_TOK + tok ))
}

total_row() {
    printf "    %-52s  %4d loc  ~%5d tokens\n" "TOTAL" "$TOTAL_LOC" "$TOTAL_TOK"
    LAST_LOC=$TOTAL_LOC; LAST_TOK=$TOTAL_TOK
    TOTAL_LOC=0; TOTAL_TOK=0
}

# ─────────────────────────────────────────────────────────────────────────────

echo ""
echo "═══════════════════════════════════════════════════════════════════"
echo "  Foster vs React — LLM surface area (apples-to-apples)"
echo "  Excludes: CSS, server boilerplate, config, generated test files"
echo "  Token estimate: chars ÷ 4"
echo "═══════════════════════════════════════════════════════════════════"

for app in counter kanban; do
    echo ""
    echo "── $app ──────────────────────────────────────────────────────────────"
    echo ""

    # ── Foster implementation ──
    echo "  Foster (implementation — machine + reducers + HTML structure)"
    case $app in
        counter)
            row "main.rs (reducers + machine, no server setup)" \
                "$(loc_rust_app "$REPO/examples/$app/src/main.rs")" \
                "$(tok_rust_app "$REPO/examples/$app/src/main.rs")"
            row "index.html (structure, no CSS)" \
                "$(loc_html "$REPO/examples/$app/static/index.html")" \
                "$(tok_html "$REPO/examples/$app/static/index.html")"
            ;;
        kanban)
            row "main.rs (reducers + machine, no server setup)" \
                "$(loc_rust_app "$REPO/examples/$app/src/main.rs")" \
                "$(tok_rust_app "$REPO/examples/$app/src/main.rs")"
            row "index.html (structure, no CSS)" \
                "$(loc_html "$REPO/examples/$app/static/index.html")" \
                "$(tok_html "$REPO/examples/$app/static/index.html")"
            ;;
    esac
    total_row
    foster_impl_loc=$LAST_LOC; foster_impl_tok=$LAST_TOK

    echo "  Foster (tests — generated from machine definition)"
    printf "    %-52s  %4s loc  ~%5s tokens\n" "*.spec.ts  [generated — not authored]" "0" "0"
    echo ""

    # ── React implementation ──
    echo "  React (implementation — component + reducer + types)"
    case $app in
        counter)
            row "App.tsx" \
                "$(loc_plain "$REPO/benchmarks/$app/react/src/App.tsx")" \
                "$(tok_plain "$REPO/benchmarks/$app/react/src/App.tsx")"
            ;;
        kanban)
            row "App.tsx" \
                "$(loc_plain "$REPO/benchmarks/$app/react/src/App.tsx")" \
                "$(tok_plain "$REPO/benchmarks/$app/react/src/App.tsx")"
            row "types.ts" \
                "$(loc_plain "$REPO/benchmarks/$app/react/src/types.ts")" \
                "$(tok_plain "$REPO/benchmarks/$app/react/src/types.ts")"
            ;;
    esac
    total_row
    react_impl_loc=$LAST_LOC; react_impl_tok=$LAST_TOK

    echo "  React (tests — hand-written, must cover transitions manually)"
    row "App.test.tsx" \
        "$(loc_plain "$REPO/benchmarks/$app/react/src/App.test.tsx")" \
        "$(tok_plain "$REPO/benchmarks/$app/react/src/App.test.tsx")"
    total_row
    react_test_loc=$LAST_LOC; react_test_tok=$LAST_TOK

    # ── delta ──
    echo ""
    impl_loc_delta=$(( foster_impl_loc - react_impl_loc ))
    impl_tok_delta=$(( foster_impl_tok - react_impl_tok ))
    total_react=$(( react_impl_tok + react_test_tok ))
    total_foster=$foster_impl_tok
    total_delta=$(( total_react - total_foster ))

    printf "  Implementation (Foster vs React):  %+d tokens  (%s)\n" \
        "$impl_tok_delta" "$([ $impl_tok_delta -gt 0 ] && echo "Foster costs more" || echo "Foster costs less")"
    printf "  Tests         (Foster vs React):  -%d tokens  (Foster tests are generated — 0 authored)\n" \
        "$react_test_tok"
    net=$(( impl_tok_delta - react_test_tok ))
    printf "  Net total:                        %+d tokens  (%s)\n" \
        "$net" "$([ $net -gt 0 ] && echo "Foster costs more overall" || echo "Foster costs less overall")"
done

echo ""
echo "═══════════════════════════════════════════════════════════════════"
echo "  Notes"
echo "  • React test coverage is manual (~70-80% of transitions)."
echo "    Foster test coverage is always 100% of edges, by construction."
echo "  • CSS excluded from both sides — equal footing."
echo "  • Server boilerplate (axum setup, tokio::main) excluded from Foster."
echo "  • React has equivalent boilerplate in package.json/vite.config —"
echo "    also excluded."
echo "  • See benchmarks/README.md for qualitative analysis."
echo "═══════════════════════════════════════════════════════════════════"
echo ""
