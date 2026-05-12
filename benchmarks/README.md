# Foster vs React — LLM Surface Area Benchmarks

## Run it

```bash
./scripts/measure.sh
```

## What is measured

**Included:** application-specific logic only.
**Excluded:** CSS (equal for both sides), server boilerplate (`fn main` / `tokio::main`), config files (`package.json`, `Cargo.toml`, `tsconfig.json`, `vite.config.ts`, `playwright.config.ts`), generated test files (`*.spec.ts`, `gen_tests.rs`).

| Side | Files counted |
|---|---|
| Foster | `main.rs` (reducers + machine, stops before `fn main`) · `index.html` (structure only, no `<style>` block) |
| React | `App.tsx` · `types.ts` · `App.test.tsx` |

Token estimate: `file_size_bytes ÷ 4`. Consistent across runs; within ~15% of tiktoken for code.

---

## Results (as of last run)

### Counter

|  | LOC | ~Tokens |
|---|---|---|
| Foster implementation | 97 | 1385 |
| Foster tests | 0 | **0** (generated) |
| React implementation | 56 | 599 |
| React tests | 61 | 626 |
| **Net Foster vs React** | | **+160 tokens** |

### Kanban

|  | LOC | ~Tokens |
|---|---|---|
| Foster implementation | 280 | 3468 |
| Foster tests | 0 | **0** (generated) |
| React implementation | 169 | 1806 |
| React tests | 71 | 796 |
| **Net Foster vs React** | | **+866 tokens** |

---

## Honest interpretation

**Foster costs more tokens to author in aggregate.** Rust + a separate HTML template is more verbose than JSX that collocates markup and logic. For the kanban app, Foster's implementation is roughly 2× the token cost of React's before tests are factored in. Generated tests recover ~800 tokens, leaving Foster ~866 tokens more expensive net.

**Where Foster wins is not token count — it's these three things:**

### 1. Test coverage is structural, not probabilistic

The React test suite covers ~8 of 10 kanban transitions — the two easiest to forget were omitted. This is typical: an LLM writing tests manually enumerates what it thinks to cover.

Foster tests are derived from `Machine::transitions()`. Every edge is covered by construction. Coverage can never drift below 100% because the test generator and the machine definition are the same artefact. There is no "forgot to test" failure mode.

### 2. Feature deltas are fully localized

Adding a new transition in Foster:
- One `.on("from", "event", "to", Some(reducer))` call
- One reducer function
- One HTML attribute (`fx-on="click->event"`)
- Run `./scripts/check.sh` — new test appears automatically

Adding a new transition in React:
- Update the action union type
- Handle the new case in the reducer switch
- Modify the component JSX (possibly in multiple places)
- Write a new test, remembering to cover it

The React change has five touch points across three files. The Foster change has three touch points in two files, and the test is not a touch point at all.

### 3. No implicit failure modes

React requires the LLM to reason correctly about:
- Hook dependency arrays (stale closures are silent bugs)
- Re-render batching (state updates may not apply immediately in event handlers)
- Key prop stability in lists (wrong keys cause subtle identity bugs)
- Test rendering lifecycle (effects may not fire in `@testing-library/react` without `act()`)

None of these exist in Foster. The state machine is a pure function of `(state, event, payload) → next_state`. If the reducer is correct, the behavior is correct. There is no hidden runtime to reason about.

---

## What would change these numbers

The implementation token gap would narrow if:
- Foster gains a JSX-like template syntax (eliminating the verbose `fx-*` attribute verbosity)
- The Rust reducers are written more concisely (currently idiomatic but verbose)
- The HTML is stripped further (comments, blank lines)

The test token gap is structural and won't change: Foster tests are always generated; React tests are always written.
