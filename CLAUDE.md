# Foster — LLM iteration guide

Everything you need to iterate on this framework. For architecture diagrams,
design rationale, and deployment see [`README.md`](README.md).

## Workspace layout

```
foster/
├── crates/
│   ├── foster-macros/     # Proc macro: html! DSL for inline templates
│   ├── foster-core/       # State machine primitives, serialization, schema validation
│   ├── foster-server/     # Axum HTTP router (4 routes) + SSE broadcast
│   ├── foster-client/     # WASM runtime — processes fx-* attributes
│   └── foster-testgen/    # Playwright test generation from machine definition
├── examples/
│   ├── counter/           # Simple idle ↔ error counter (port 3000)
│   ├── player/            # 6-state media player (port 3001)
│   ├── kanban/            # Multi-column task board with fx-for (port 3002)
│   └── aura/              # CSS animation showcase with fx-class (port 3003)
└── scripts/
    ├── build-wasm.sh      # Build foster-client and move pkg/ to workspace root
    ├── demo.sh            # Start one or all demo servers (Ctrl-C stops all)
    └── gen-tests.sh       # Regenerate Playwright specs for all examples
```

`Cargo.toml` includes native crates only. `crates/foster-client` targets
`wasm32-unknown-unknown` and must be built separately with `wasm-pack` via
`./scripts/build-wasm.sh`.

## Iteration loop

The primary feedback signal is a **failing Playwright assertion** — state names
and event names in the test exactly match the machine definition.

### Tight loop (type-check + unit tests, no browser)

```bash
./scripts/check.sh          # cargo check + cargo test + gen_tests (all examples)
./scripts/check.sh kanban   # same, one example only
```

### Full loop (browser)

```bash
./scripts/demo.sh kanban    # start server (separate terminal)
cd examples/kanban && npx playwright test --reporter=line
```

### Adding a feature — minimal change

1. Add a transition in `src/main.rs`:
   `.on("from", "event", "to", reducer_fn)` or `.pass(...)` for no-op
2. Add the reducer (if not a passthrough)
3. Add the `html!` element: `button[on="click->event_name"] { "label" }`
4. Run `./scripts/check.sh` — tests for the new edge appear automatically

No test file to edit. No type file to update. The machine definition is the
single source of truth; tests and template validation are derived.

### What good iteration looks like

```
Write machine definition
  → ./scripts/check.sh  (type-safe, unit tests green, specs regenerated)
  → npx playwright test
  → One test fails: "kanban | viewing →[move_task]→ viewing"
  → The failure names the exact state/event/transition that's wrong
  → Fix the reducer, re-run, green
```

## State machine API (foster-core)

```rust
MachineBuilder::new("counter", "idle", json!({ "count": 0 }))
    .state("error")
    .schema("idle", json!({          // optional — validated on every entry
        "type": "object",
        "required": ["count"],
        "properties": { "count": { "type": "integer", "minimum": 0 } }
    }))
    .on("idle", "increment", "idle", |ctx, _| {
        Ok(json!({ "count": ctx["count"].as_i64().unwrap_or(0) + 1 }))
    })
    .pass("idle",  "break_it", "error")   // context unchanged
    .pass("error", "recover",  "idle")
    .template(page("Counter", include_str!("../static/style.css"), html! {
        div[machine="counter"] {
            div[show="idle"] {
                span[text="count"] {}
                button[on="click->increment"] { "+" }
            }
            div[show="error"] {
                button[on="click->recover"] { "recover" }
            }
        }
    }))
    .build()  // → Arc<Machine>
```

### Transition methods

| Method | When to use |
|--------|-------------|
| `.on(from, event, to, reducer)` | Reducer transforms `Value` context directly |
| `.pass(from, event, to)` | Context passes through unchanged |
| `.typed_on(from, event, to, reducer)` | Reducer works with a typed struct — no `json!` unwrapping |

`.typed_on` avoids `json!` reconstruction for complex context:

```rust
#[derive(Serialize, Deserialize, Clone, Default)]
struct KanbanCtx { tasks: Vec<Task>, draft_title: String, editing_id: String }

fn begin_edit(mut ctx: KanbanCtx, payload: Value) -> Result<KanbanCtx, MachineError> {
    ctx.editing_id  = payload["id"].as_str().unwrap_or("").to_string();
    ctx.draft_title = ctx.tasks.iter().find(|t| t.id == ctx.editing_id)
                          .map(|t| t.title.clone()).unwrap_or_default();
    Ok(ctx)
}

builder.typed_on("viewing", "start_edit", "editing", begin_edit)
```

### Key types

- `Machine` — static, `Arc`-shared; reducers are `Arc<dyn Fn>` so closures, fn pointers, and typed wrappers all work
- `MachineInstance` — mutable runtime state: current state + context `Value` + monotonic version
- `Snapshot` — unit of everything: wire format, test injection, state diffing

**Invariant: state transitions are the only way state changes.** The server owns instances; the client is a render layer.

### Inline templates — `html!` + `page()`

`foster_core::html!` is a compile-time DSL that generates HTML strings with shorthand `fx-*` attributes:

```rust
use foster_core::{html, page};

// Shorthand → HTML attribute
// machine="id"   → fx-machine="id"
// show="states"  → fx-show="states"
// text="key"     → fx-text="key"
// on="evt->act"  → fx-on="evt->act"
// each="key"     → fx-for="key"
// filter=r#"..."# → fx-where="..."
// collect="key"  → fx-collect="key"
// disable="s"    → fx-disable="s"
// value="key"    → fx-value="key"
// payload=r#"..."# → fx-payload="..."
// field="key"    → fx-field="key"
// state_label    → fx-state-label (boolean)
// foo_bar="v"    → foo-bar="v"  (underscore → hyphen for everything else)

.template(page("My App", include_str!("../static/style.css"), html! {
    div[machine="counter"] {
        span[state_label] {}
        div[show="idle"] {
            span[text="count"] { "0" }
            button[on="click->increment"] { "+" }
        }
        div[show="error"] {
            button[on="click->recover"] { "recover" }
        }
    }
}))
```

`page(title, style, body)` wraps the body in a complete HTML shell: DOCTYPE, `<head>`, the `[fx-show]{display:none}` rule, the CSS, and the WASM `<script>` tag. Raw string literals (`r#"..."#`) are useful for JSON in `filter=` and `payload=` attributes.

### Template validation

`.template(html)` causes `foster_server::router()` to:
1. Serve it at `GET /` automatically
2. Panic at startup if any `fx-show` or `fx-on` value references an unknown state or event

```rust
// Panics: "event 'incremnt' not defined in machine 'counter'"
.template(r#"<button fx-on="click->incremnt">+</button>"#)
```

### Schema validation

`.schema(state, json_schema)` — validated on every state entry (both `send()` and `restore()`).
Supported keywords: `type`, `required`, `properties`, `minimum`, `maximum`, `minLength`, `maxLength`, `enum`.
Returns `MachineError::SchemaViolation` before any state is committed.
Inlined — no external dependencies, compiles to WASM.

## HTTP API (foster-server)

| Method | Path | Format | Purpose |
|--------|------|--------|---------|
| GET | `/state?machine=<id>&session=<sid>` | MessagePack | Current snapshot |
| POST | `/transition` | MessagePack in/out | Fire event, get new snapshot |
| GET | `/events?machine=<id>&session=<sid>` | SSE (JSON) | Push stream |
| POST | `/test/state?session=<sid>` | JSON in/out | Inject snapshot (debug only) |
| GET | `/debug/history?machine=<id>&session=<sid>` | JSON | History ring buffer — up to 50 snapshots, oldest first (debug only) |
| POST | `/debug/rewind?machine=<id>&session=<sid>&version=N` | JSON | Restore a historical snapshot and broadcast via SSE (debug only) |
| GET | `/debug/graph?machine=<id>&session=<sid>` | HTML | Self-contained state graph visualiser — SVG nodes/edges + live SSE state highlight (debug only) |

`session` defaults to `"default"` if omitted.

```bash
curl -X POST 'http://localhost:3000/test/state?session=my-test' \
     -H 'Content-Type: application/json' \
     -d '{"machine_id":"counter","state":"error","context":{"count":99},"version":0}'
```

`POST /test/state` returns 403 in release builds unless `FOSTER_TEST_MODE=1` is set.

## Session isolation

Every machine instance is keyed by `(session_id, machine_id)`, created lazily on first access.
The WASM client generates a 128-bit random UUID if none is in the URL and stamps it as
`data-fx-session` on `[fx-machine]`:

```typescript
const sid = await root.getAttribute('data-fx-session');
await request.post(`/test/state?session=${sid}`, { data: { ... } });
```

## HTML template DSL (fx-* attributes)

All attributes processed client-side by the WASM runtime.

| Attribute | Example | Meaning |
|-----------|---------|---------|
| `fx-machine` | `fx-machine="counter"` | Root; stamped with `data-fx-state`, `data-fx-version`, `data-fx-session` |
| `fx-show` | `fx-show="idle,loading"` | Visible only in listed states |
| `fx-text` | `fx-text="count"` | Set text from `context[key]` |
| `fx-disable` | `fx-disable="loading"` | Add `disabled` in listed states |
| `fx-on` | `fx-on="click->increment"` | Fire machine event on DOM event |
| `fx-state-label` | `fx-state-label` | Display current state name |
| `fx-for` | `fx-for="tasks"` | Clone first child per item in `context[key]` |
| `fx-where` | `fx-where='{"column":"todo"}'` | Filter `fx-for` items by key/value |
| `fx-field` | `fx-field="title"` | Set text from item field inside `fx-for` |
| `fx-collect` | `fx-collect="draft_title"` | Read input value into transition payload |
| `fx-value` | `fx-value="draft_title"` | Pre-populate input from context |
| `fx-payload` | `fx-payload='{"col":"done"}'` | Static JSON merged into transition payload |
| `fx-class` | `fx-class="calm:is-active"` | Add CSS class when in named state |
| `fx-bind-attr` | `fx-bind-attr="href=ctx:url"` | Bind HTML attribute from context or state |

**`fx-bind-attr` format:** space-separated `attr=source:value` pairs.
- `attr=ctx:key` — set from `context[key]`; removes attr if key absent
- `attr=state:name` — set `attr=""` when in that state (use for `disabled`, `hidden`, `aria-current`)

CSS convention: `[fx-show] { display: none; }` hides showable elements before WASM loads.

## Playwright integration

```typescript
const root = page.locator('[fx-machine="counter"]');
const sid  = await root.getAttribute('data-fx-session');

await expect(root).toHaveAttribute('data-fx-state', 'idle');

// Inject state — SSE pushes it immediately, no reload needed
await request.post(`/test/state?session=${sid}`, {
  data: { machine_id: 'counter', state: 'error', context: { count: 99 }, version: 0 }
});
await expect(root).toHaveAttribute('data-fx-state', 'error');

await page.locator('[fx-on="click->recover"]').first().click();
await expect(root).toHaveAttribute('data-fx-state', 'idle');
```

### Test generation

`foster-testgen` derives the full suite from the machine graph. Four suites, nothing written by hand:

| Suite | Catches |
|-------|---------|
| **Transition coverage** (1 per edge) | Missing or broken reducers |
| **Multi-step walk** (1 test, visits every state ≥2×) | SSE ordering bugs, stale `data-fx-state` |
| **Rapid toggle pairs** (1 per bidirectional pair, 4× each) | `fx-class` / animation sync bugs |
| **Snapshot injection** (1 per state) | Broken `POST /test/state` |

```bash
./scripts/gen-tests.sh                      # all examples
cargo run -p aura --bin gen_tests           # one example
```

The generated `injectState` helper waits for WASM bootstrap before injecting,
so the SSE listener is always wired before state is pushed.

## Roadmap — what to build next

When asked to add a feature, check here first so your design is consistent with planned work.

### Pending items

| Item | Design notes |
|------|-------------|
| **HA / multi-replica** | `StateStore` + `PubSub` traits are already defined in `crates/foster-server/src/store.rs`. Implement `RedisStore` and `RedisPubSub` using `redis-rs`. `AppState` should become generic over both traits. The `version` field on `Snapshot` is the optimistic lock token — use a Redis Lua CAS script or `WATCH`/`MULTI` to reject stale writes. |
| **Time-travel debugger** | ~~Ring buffer of snapshots in `MachineInstance` (cap ~50). Expose `GET /debug/history?session=<sid>&machine=<id>` returning `Vec<Snapshot>`. Add `POST /rewind?version=N` that calls `restore()`. Store trait will need a `history()` method for the Redis path.~~ **Done** — `InMemoryStore` tracks a 50-entry ring buffer per `(session, machine)`. `StateStore` trait has `history()`. `GET /debug/history?session=<sid>&machine=<id>` returns `Vec<Snapshot>` JSON. `POST /debug/rewind?session=<sid>&machine=<id>&version=N` restores a historical snapshot and broadcasts via SSE. Both routes gated by `test_mode`. |
| **State graph UI** | ~~`GET /debug/graph` returns a self-contained HTML page. Use D3 force layout SVG. Nodes = states, edges = events. Highlight current state per session via SSE. Add only in debug builds / behind `FOSTER_DEV_UI=1`.~~ **Done** — `GET /debug/graph?machine=<id>&session=<sid>` returns a self-contained HTML page. Circular SVG layout with arrowhead edges (curves for bidirectional pairs, arcs for self-loops). Initial state shown with a dashed outer ring. SSE connection highlights the current state in green. Gated by `test_mode`. |
| **Dev overlay** | ~~Floating panel injected via `<script>` tag in debug builds.~~ **Done** — Implemented in Rust/WASM (`crates/foster-client/src/lib.rs`), compiled with `debug_assertions`. `mount_overlay()` creates the panel DOM in Rust after WASM bootstrap; `update_overlay()` called on every snapshot. Server injects only `window.__FOSTER_MACHINES` (machine state names, JSON, no logic) for the jump-to-state dropdown. Minimize state persisted in `sessionStorage`. `Snapshot.last_event: Option<String>` set by `send()`, `None` for `restore()`. |
| **Multiple machines per page** | Instance addressing syntax: `fx-machine="counter#1"`. The `#fragment` becomes the instance key appended to the session. WASM client needs to split on `#` when building the session key. |
| **Generated TypeScript SDK** | Derive typed `sendEvent(event: CounterEvent, payload?: ...) → Promise<Snapshot>` and `setState(snap: Snapshot)` from the machine definition. Output alongside the Playwright spec in `gen_tests`. |
| **Compiled machine validation** | Proc-macro that turns a `MachineBuilder` into a compile-time-checked type graph: Rust enum for states, exhaustive `match` on events. Catches unreachable states and missing transitions at compile time. |
| **Differential rendering** | Server sends a JSON Patch (`RFC 6902`) diff of context instead of the full snapshot. Reduces wire payload for large context objects (e.g. kanban task lists). WASM client applies the patch with `json-patch`. |

### Already implemented

- `StateStore` / `PubSub` traits + `InMemoryStore` / `InMemoryPubSub` impls — `crates/foster-server/src/store.rs`
- Time-travel debugger: `GET /debug/history` + `POST /debug/rewind`, 50-entry ring buffer in `InMemoryStore`, `history()` on `StateStore` trait — `crates/foster-server/src/`
- State graph UI: `GET /debug/graph` — self-contained SVG visualiser with live SSE state highlighting — `crates/foster-server/src/lib.rs`
- Dev overlay: Rust/WASM panel in `crates/foster-client/src/lib.rs` — compiled with `debug_assertions`; server injects only `window.__FOSTER_MACHINES` metadata (no JS logic)
- `Snapshot.last_event: Option<String>` — set by `MachineInstance::send()`, `None` for `restore()` and initial state — `crates/foster-core/src/`

## Security invariants — do not break

- **All state transitions go through `machine.send()`** — never mutate `MachineInstance` directly
- **`POST /test/state` is debug-only** — gated by `cfg(debug_assertions)` or `FOSTER_TEST_MODE=1`; do not widen this
- **Session keys are random UUIDs** — do not add any endpoint that accepts a user-supplied session ID without validation
- **Schema validation runs on both `send()` and `restore()`** — do not add a path that bypasses it
