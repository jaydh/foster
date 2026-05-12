# Foster vs React — LLM Surface Area Benchmarks

## Run it

```bash
./scripts/measure.sh
```

The React implementations are embedded inline in `measure.sh` — not committed as separate files. The script writes them to a temp dir, measures, and cleans up.

## What is measured

**Included:** application-specific logic only.
**Excluded:** CSS, server boilerplate (axum setup, `tokio::main`, `HashMap` wiring), config files (`package.json`, `Cargo.toml`, `tsconfig.json`), generated test files (`*.spec.ts`, `gen_tests.rs`), `use`/`import` statements.

| Side | Files counted |
|---|---|
| Foster | `main.rs` — structs, reducers, machine builder + `html!` template; stops before `let mut machines = HashMap::new()` |
| React | `App.tsx` · `types.ts` · `App.test.tsx` |

Token estimate: `file_size_bytes ÷ 4`. Consistent across runs; within ~15% of tiktoken for code.

---

## Results (as of last run)

### Counter

|  | LOC | ~Tokens |
|---|---|---|
| Foster implementation | 43 | 544 |
| Foster tests | 0 | **0** (generated) |
| React implementation | 56 | 515 |
| React tests | 61 | 590 |
| **Net Foster vs React** | | **−561 tokens** |

### Kanban

|  | LOC | ~Tokens |
|---|---|---|
| Foster implementation | 182 | 2209 |
| Foster tests | 0 | **0** (generated) |
| React implementation | 165 | 1671 |
| React tests | 70 | 725 |
| **Net Foster vs React** | | **−187 tokens** |

---

## Interpretation

**Foster costs fewer tokens to author in aggregate.** Implementation is slightly more expensive on complex apps (typed Rust structs + reducers take more lines than TypeScript arrow functions) but generated tests more than offset that.

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
- One `html!` attribute (`button[on="click->event"]`)
- Run `./scripts/check.sh` — new test appears automatically

Adding a new transition in React:
- Update the action union type
- Handle the new case in the reducer switch
- Modify the component JSX (possibly in multiple places)
- Write a new test, remembering to cover it

### 4. No implicit failure modes

React requires the LLM to reason correctly about hook dependency arrays, re-render batching, key prop stability, and test rendering lifecycle. None of these exist in Foster. The state machine is a pure function of `(state, event, payload) → next_state`.

### 5. Inline templates eliminate file-switching

With `html!` + `page()`, the entire app — state machine, reducers, and template — lives in one `main.rs`. No separate HTML file to keep in sync, no risk of a template referencing an event that was renamed in the machine.
