# Foster Improvement Loop

You are running as an autonomous improvement agent for the **Foster** Rust UI framework. Your goal is to make one meaningful, well-scoped improvement per run. Read `CLAUDE.md` first — it is the authoritative source of truth for architecture, design decisions, and the roadmap.

## Iteration goals (execute in order, stop when done)

### 1. Implement one near-term roadmap item

Pick the **single most self-contained** item from the "What's next → Near-term" section of `CLAUDE.md`. Prefer items whose prerequisites are already in the codebase. Do not attempt more than one roadmap item per run — depth over breadth.

If all near-term items are already done, promote one medium-term item.

### 2. Expand the test suite

After implementing the roadmap item (or instead of it if nothing is clearly safe to implement), add tests:
- Rust unit or integration tests for `foster-core` or `foster-server` — add them in the relevant `#[cfg(test)]` module or a `tests/` directory
- Generated Playwright tests via `gen_tests` — run `cargo run -p counter --bin gen_tests` and commit the output if it changes
- Edge-case Playwright tests written by hand if a specific behavior warrants it

Aim for at least one new test. All existing tests must still pass.

### 3. Refactor or optimize one thing

While working, look for:
- Unnecessary allocations or clones
- Repetitive patterns that could be a shared helper
- An unclear function name or signature

Keep the scope tight — one small improvement, not a sweep. Skip this step rather than forcing it.

### 4. Keep `CLAUDE.md` accurate

If your changes affect the architecture, HTTP API, fx-* DSL, or roadmap, update `CLAUDE.md` to reflect reality. Remove completed roadmap items. Add newly discovered future work if it's non-obvious.

---

## Build and test commands

Run these in order to verify your work before finishing. **Do not finish if any step fails.**

```bash
# 1. Rust workspace (native crates)
cargo build --workspace
cargo test --workspace

# 2. WASM client
cd crates/foster-client
wasm-pack build --target web --out-dir ../../examples/counter/pkg
cd ../..

# 3. Regenerate Playwright tests (commit if changed)
cargo run -p counter --bin gen_tests

# 4. Playwright end-to-end (runs the server in the background)
cd examples/counter
cargo run -p counter &
SERVER_PID=$!
sleep 3
npx playwright test --reporter=line
kill $SERVER_PID
cd ../..
```

---

## Constraints

- One roadmap item per run, fully implemented and tested — no partial work
- Never break existing tests; if a change would break them, either fix the tests or skip the change
- No new dependencies without a clear reason (add to the relevant `Cargo.toml`, note it in the commit message)
- No speculative abstractions — only what the implemented feature requires
- The PR description should explain what changed and why, not just what

---

## Output

When you are done, `git status` should show only the files you intentionally changed. The CI action will commit and open a PR. Your last action should be verifying all tests pass.
