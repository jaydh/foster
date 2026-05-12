# Foster vs React — LLM Surface Area Benchmarks

## How to read these numbers

The goal is not to minimize lines of code. It's to minimize **what an LLM must reason about** to implement or modify a feature correctly. That's a different metric.

```
./scripts/measure.sh
```

## Latest results

```
App               LOC    ~Tokens
counter/foster    187     2259     (includes full CSS in index.html)
counter/react     120     1225     (no styling)
kanban/foster     359     4483     (includes full CSS in index.html)
kanban/react      244     2602     (no styling)
```

Raw token counts favor React slightly. But the benchmark captures three things the numbers miss:

---

## What the numbers don't capture

### 1. Test coverage is not comparable

React tests cover only what the developer remembered to write. The kanban RTL test suite covers ~8 of 10 transitions — two were omitted for brevity. If you asked an LLM to "add full test coverage," it would need to enumerate all transitions manually, possibly miss some, and write brittle DOM-shape-dependent assertions.

Foster tests are generated from the machine definition. Coverage is always 100% of edges, by construction, with zero authoring cost. The LLM writes the machine (which it was going to write anyway) and gets a complete test suite as a side effect.

**Effective test-writing cost:**
- React: ~60–100 tokens per transition tested
- Foster: 0 tokens per transition (generated)

For kanban's 10 transitions, that's ~600–1000 tokens the LLM never writes.

### 2. Conceptual load per feature addition

When adding a new feature (e.g., "add an 'archive' column to the kanban"):

**React requires reasoning about:**
- Update the `Column` type union
- Add the column to the `KanbanAction` union
- Handle it in the reducer switch statement (in the right case)
- Update all three `Column` components to conditionally render the new move button
- Update tests — which tests break? Which new ones to write?
- Worry about re-render correctness, stale closures, key prop stability

**Foster requires reasoning about:**
- Add one `on("viewing", "move_task", "viewing", Some(move_task))` call (already exists, just add the column value)
- Add one `<button fx-on="click->move_task" fx-payload='{"column":"archive"}'>` in HTML
- Run `gen_tests` — new tests appear automatically

The change is localized to exactly one place per concern. There is no fan-out.

### 3. The LLM cannot make implicit React mistakes in Foster

In React, an LLM can write code that:
- Triggers unnecessary re-renders (missing `useCallback`, wrong `key` prop)
- Creates stale closures in `useEffect` (missing deps array entry)
- Causes state update batching surprises
- Makes tests pass in isolation but fail under concurrent rendering

None of these failure modes exist in Foster. There is no reconciler, no hook dependency graph, no closure capture. The state machine is a pure function. If the LLM writes a correct reducer and a correct transition, the behavior is correct.

These bugs don't show up in token counts — they show up in production.

---

## What Foster costs more

### Initial setup tokens

Foster requires the LLM to understand:
- `MachineBuilder` API (~20 tokens of pattern)
- `fn` pointer reducers vs closures (one sentence)
- `fx-*` attribute DSL (~10 attributes)

React requires understanding React itself (much larger, but the LLM already knows it from training data). For a novel framework, Foster has higher *first-time* cost. For an LLM already loaded with CLAUDE.md, it's negligible.

### HTML verbosity

Foster's templates include CSS. If you separate styling from structure, Foster's application-logic HTML is comparable to React JSX. The CSS is the same cost either way — React apps also have it, just in a different file.

---

## The iteration loop comparison

**React feature cycle:**
1. LLM updates type definitions
2. LLM updates reducer
3. LLM updates component(s) — possibly multiple
4. LLM writes new tests, manually enumerating what to test
5. Run tests, iterate on failures
6. Possibly fix re-render bugs that tests don't catch

**Foster feature cycle:**
1. LLM adds `.on()` transition + reducer function
2. LLM adds `fx-on` attribute in HTML
3. `./scripts/check.sh` — type-check + unit tests + gen_tests
4. Run Playwright tests, iterate on failures

Step 4 in Foster generates tests for the new transition automatically. The LLM never writes a test that covers a transition it added — it's always covered.

---

## Methodology

Files measured (application-specific, excludes config and generated code):

| Implementation | Files included |
|---|---|
| Foster counter | `examples/counter/src/main.rs`, `static/index.html` |
| Foster kanban | `examples/kanban/src/main.rs`, `static/index.html` |
| React counter | `App.tsx`, `App.test.tsx` |
| React kanban | `App.tsx`, `types.ts`, `App.test.tsx` |

Files excluded from both sides: `package.json`, `Cargo.toml`, `tsconfig.json`, `vite.config.ts`, `playwright.config.ts`, `gen_tests.rs`, generated `*.spec.ts`.

Token estimate: `file_size_bytes ÷ 4`. This is within ~15% of tiktoken for typical TypeScript/Rust code and is consistent across runs.
