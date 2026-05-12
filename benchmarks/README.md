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
| Foster implementation | 69 | 860 |
| Foster tests | 0 | **0** (generated) |
| React implementation | 56 | 599 |
| React tests | 61 | 626 |
| **Net Foster vs React** | | **−365 tokens** |

### Kanban

|  | LOC | ~Tokens |
|---|---|---|
| Foster implementation | 201 | 2484 |
| Foster tests | 0 | **0** (generated) |
| React implementation | 169 | 1806 |
| React tests | 71 | 796 |
| **Net Foster vs React** | | **−118 tokens** |

---

## Interpretation

**Foster costs fewer tokens to author in aggregate.** The HTML template is still the largest single cost on the Foster side — JSX collocates markup and logic while Foster separates them — but typed context structs, inline closures, and generated tests more than offset that.

**Where Foster wins:**

### 1. Test coverage is structural, not probabilistic

The React test suite covers ~8 of 10 kanban transitions — the two easiest to forget were omitted. This is typical: an LLM writing tests manually enumerates what it thinks to cover.

Foster tests are derived from `Machine::transitions()`. Every edge is covered by construction. Coverage can never drift below 100% because the test generator and the machine definition are the same artefact.

### 2. Template errors surface at startup, not at runtime

`foster_server::router()` validates every `fx-show` state name and `fx-on` event name at server startup. A typo panics immediately:

```
Machine 'counter' template validation failed:
fx-on="click->incremnt": event 'incremnt' not defined in machine 'counter'
```

React has no equivalent — a misnamed handler silently does nothing.

### 3. Feature deltas are fully localized

Adding a new transition in Foster:
- One `.on()` / `.pass()` / `.typed_on()` call
- One reducer (or inline closure for simple cases)
- One HTML attribute (`fx-on="click->event"`)
- Run `./scripts/check.sh` — new test appears automatically

Adding a new transition in React:
- Update the action union type
- Handle the new case in the reducer switch
- Modify the component JSX (possibly in multiple places)
- Write a new test, remembering to cover it

### 4. No implicit failure modes

React requires the LLM to reason correctly about hook dependency arrays, re-render batching, key prop stability, and test rendering lifecycle. None of these exist in Foster. The state machine is a pure function of `(state, event, payload) → next_state`.

---

## What would change these numbers

The implementation token gap would narrow further if Foster gains a JSX-like template syntax — the HTML file is the dominant remaining cost.
