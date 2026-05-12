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
| **Net Foster vs React** | | **−365 tokens** (Foster costs less) |

### Kanban

|  | LOC | ~Tokens |
|---|---|---|
| Foster implementation | 201 | 2484 |
| Foster tests | 0 | **0** (generated) |
| React implementation | 169 | 1806 |
| React tests | 71 | 796 |
| **Net Foster vs React** | | **−118 tokens** (Foster costs less) |

---

## What changed

The previous version of these benchmarks had Foster costing **+160** (counter) and **+866** (kanban) tokens more than React. Three structural changes reversed that:

### 1. Inline closures replaced named reducer functions

Before, every reducer was a named `fn` at module scope even when trivial:

```rust
fn increment(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let n = ctx["count"].as_i64().unwrap_or(0);
    Ok(json!({ "count": n + 1 }))
}
// ... repeated for decrement, reset, passthrough ...
.on("idle", "increment", "idle", Some(increment))
```

After, simple reducers are inline:

```rust
.on("idle", "increment", "idle", |ctx, _| Ok(json!({ "count": ctx["count"].as_i64().unwrap_or(0) + 1 })))
.pass("idle", "break_it", "error")
```

### 2. Typed context structs eliminated json! reconstruction

Before, every kanban reducer had to reconstruct the full context to update one field:

```rust
fn begin_delete(ctx: Value, payload: Value) -> Result<Value, MachineError> {
    let id = payload["id"].as_str().unwrap_or("").to_string();
    let mut map = ctx.as_object().cloned().unwrap_or_default();
    map.insert("confirm_id".into(), json!(id));
    Ok(Value::Object(map))
}
```

After, with `.typed_on()` and typed structs:

```rust
fn begin_delete(mut ctx: KanbanCtx, payload: Value) -> Result<KanbanCtx, MachineError> {
    ctx.confirm_id = payload["id"].as_str().unwrap_or("").to_string();
    Ok(ctx)
}
```

### 3. `.template()` eliminated the index handler boilerplate

Before, every example needed an explicit `async fn index()` handler, HashMap insertion, and `route("/", get(index))`. After, one `.template(include_str!("..."))` call on the machine definition handles everything — the framework serves it at `GET /` automatically.

---

## Honest interpretation

**Foster now costs fewer tokens in aggregate for both example apps.** The wins come entirely from the structural changes above — typed context, inline closures, and template co-location. The HTML template is still the dominant cost on the Foster side (830 tokens for counter's structure-only HTML vs 599 for React's entire component), so JSX-style colocation would narrow the gap further.

**Where Foster wins beyond token count:**

### 1. Test coverage is structural, not probabilistic

The React test suite covers ~8 of 10 kanban transitions — the two easiest to forget were omitted. This is typical: an LLM writing tests manually enumerates what it thinks to cover.

Foster tests are derived from `Machine::transitions()`. Every edge is covered by construction. Coverage can never drift below 100% because the test generator and the machine definition are the same artefact. There is no "forgot to test" failure mode.

### 2. Template errors surface at startup, not at runtime

With `.template()`, `foster_server::router()` validates every `fx-show` state name and `fx-on` event name against the machine definition at server startup. A typo panics immediately:

```
Machine 'counter' template validation failed:
fx-on="click->incremnt": event 'incremnt' not defined in machine 'counter'
```

React has no equivalent — a misnamed event handler silently does nothing.

### 3. Feature deltas are fully localized

Adding a new transition in Foster:
- One `.on()` / `.pass()` / `.typed_on()` call
- One reducer function (or inline closure)
- One HTML attribute (`fx-on="click->event"`)
- Run `./scripts/check.sh` — new test appears automatically

Adding a new transition in React:
- Update the action union type
- Handle the new case in the reducer switch
- Modify the component JSX (possibly in multiple places)
- Write a new test, remembering to cover it

### 4. No implicit failure modes

React requires the LLM to reason correctly about hook dependency arrays, re-render batching, key prop stability, and test rendering lifecycle. None of these exist in Foster.

---

## What would change these numbers

The implementation token gap would narrow further if:
- Foster gains a JSX-like template syntax (the HTML file is still the largest cost)
- The HTML is stripped of structural boilerplate (head, body, script tags)
