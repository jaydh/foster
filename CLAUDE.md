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
│   ├── aura/              # CSS animation showcase with fx-class (port 3003)
│   ├── checkout/          # 7-state checkout flow, showcases graph + history (port 3004)
│   ├── plane/             # Linear-style issue tracker — 5 states, 15 transitions (port 3005)
│   └── notion/            # Notion-style block editor — 2 states, 10 transitions (port 3006)
└── scripts/
    ├── build-wasm.sh      # Build foster-client (release); pass --dev for dev overlay
    ├── demo.sh            # Build WASM (dev) + start all demo servers (Ctrl-C stops all)
    └── gen-tests.sh       # Regenerate Playwright specs for all examples (counter player kanban aura checkout plane notion)
```

`Cargo.toml` includes native crates only. `crates/foster-client` targets
`wasm32-unknown-unknown` and must be built separately with `wasm-pack`.
`./scripts/demo.sh` handles this automatically — it builds dev WASM then starts servers.

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
./scripts/demo.sh kanban    # builds dev WASM + starts server (separate terminal)
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
| GET | `/debug/timeline?machine=<id>&session=<sid>` | HTML | History replay timeline — scrub through snapshots, auto-play, live tail (debug only) |
| GET | `/debug/benchmark?machine=<id>` | JSON | Walk machine graph in-memory; report full-snapshot vs JSON Patch sizes per transition (debug only) |

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
| `fx-if` | `fx-if="error_msg"` | Show when `context[key]` is truthy; supports comparison object |
| `fx-animate` | `fx-animate="error:shake:400"` | Add CSS class for N ms when entering a state |

**`fx-bind-attr` format:** space-separated `attr=source:value` pairs.
- `attr=ctx:key` — set from `context[key]`; removes attr if key absent
- `attr=state:name` — set `attr=""` when in that state (use for `disabled`, `hidden`, `aria-current`)

**`fx-if` format:**
- `fx-if="field"` — show when `context[field]` is truthy (non-null, non-false, non-zero, non-empty)
- `fx-if='{"field":"count","op":"gt","value":0}'` — comparison; ops: `eq neq gt lt gte lte`

**`fx-animate` format:** space-separated `state:class:duration_ms` specs.
- `fx-animate="confirmed:pop-in:600"` — adds `pop-in` class for 600 ms when entering `confirmed`
- `fx-animate="*:flash:200"` — fires on every state transition
- Multiple: `fx-animate="error:shake:400 confirmed:pop-in:600"`

CSS convention: `[fx-show] { display: none; }` hides showable elements before WASM loads.

**Session persistence:** The WASM client stores the session ID in `localStorage["foster_session"]` so state survives page reloads. Resolution order: URL `?session=` param → localStorage → new UUID (persisted). Each origin (host + port) has an independent localStorage scope.

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

No pending items — all planned features are implemented. See "Already implemented" below.

### Already implemented

- `StateStore` / `PubSub` traits + `InMemoryStore` / `InMemoryPubSub` impls — `crates/foster-server/src/store.rs`
- Time-travel debugger: `GET /debug/history` + `POST /debug/rewind`, 50-entry ring buffer in `InMemoryStore`, `history()` on `StateStore` trait — `crates/foster-server/src/`
- State graph UI: `GET /debug/graph` — self-contained SVG visualiser with live SSE state highlighting — `crates/foster-server/src/lib.rs`
- Dev overlay: Rust/WASM panel in `crates/foster-client/src/lib.rs` — compiled with `debug_assertions`; server injects only `window.__FOSTER_MACHINES` metadata (no JS logic); CSS injected server-side so no flash on load
- `Snapshot.last_event: Option<String>` — set by `MachineInstance::send()`, `None` for `restore()` and initial state — `crates/foster-core/src/`
- Differential rendering: SSE emits named `snapshot` (full, on connect) + `patch` (RFC 6902 JSON Patch) events — `crates/foster-server/src/lib.rs`; WASM client applies patches via `json-patch` crate — `crates/foster-client/src/lib.rs`
- Checkout example: 7-state checkout flow (`cart → shipping → payment → review → processing → confirmed/failed`), port 3004 — `examples/checkout/`
- `demo.sh` is self-sufficient: builds dev WASM then starts all servers; `<link rel="icon" href="data:,">` in HTML shell eliminates favicon 404s
- Multiple machines per page: `fx-machine="counter#1"` / `fx-machine="counter#2"` — fragment appended to session with `.` separator; context cache and dev overlay panels are keyed per instance — `crates/foster-client/src/lib.rs`
- Generated TypeScript SDK: `foster_testgen::generate_sdk` emits `tests/{name}.sdk.ts` alongside each Playwright spec. Exports `{Name}State`, `{Name}Event`, `{Name}Snapshot`, and `{Name}Client` (with `sendEvent`, `getState`, `setState`). `@msgpack/msgpack` added to all example `devDependencies` — `crates/foster-testgen/src/lib.rs`
- Compiled machine validation: `machine_graph!` proc-macro in `crates/foster-macros/src/lib.rs`, re-exported from `foster-core`. Accepts `{id, initial, states, transitions}` block. Emits `compile_error!` for unknown states in transitions or unreachable states. Generates `{PascalId}State` + `{PascalId}Event` enums with `as_str()`. Used in `examples/counter/src/main.rs`.
- Generic `AppState<S, P>` + `router_with()`: generic over `S: StateStore + Clone + 'static` and `P: PubSub + Clone + 'static` with default type params. HTTP integration tests in `crates/foster-server/src/lib.rs`. `RedisStore` + `RedisPubSub` behind `--features redis-backend` in `crates/foster-server/src/store.rs`.
- History replay timeline: `GET /debug/timeline?machine=<id>&session=<sid>` — self-contained HTML page with horizontal scrollable snapshot rail, ◀ / ▶ step controls, auto-play with configurable speed, live SSE tail, and split bottom panel: **live UI preview iframe** (left, shows the actual app UI at the selected snapshot via isolated `{session}__tl` preview session + `POST /test/state` injection) and context JSON (right, 320 px). Overlay "history" link updated to point here. Gated by `test_mode` — `crates/foster-server/src/lib.rs`.
- `plane` and `notion` added to `gen-tests.sh` and `check.sh` gen_tests loop so their Playwright specs are regenerated on every `./scripts/check.sh` run.
- `fx-if` — context-conditional visibility: `fx-if="field"` (truthy check) or `fx-if='{"field":"f","op":"eq|neq|gt|lt|gte|lte","value":...}'` — `crates/foster-client/src/lib.rs`.
- `fx-animate` — timed CSS class on state enter: `fx-animate="state:class:ms"`, `*` for any state — `crates/foster-client/src/lib.rs`.
- Session persistence — `resolve_session_id` now persists the session UUID to `localStorage["foster_session"]`; URL `?session=` still takes precedence (Playwright / timeline preview) — `crates/foster-client/src/lib.rs`.
- `GET /debug/benchmark` — BFS walk of machine graph in-memory; reports full-snapshot vs JSON Patch bytes per transition and overall ratio — `crates/foster-server/src/lib.rs`.

## Security invariants — do not break

- **All state transitions go through `machine.send()`** — never mutate `MachineInstance` directly
- **`POST /test/state` is debug-only** — gated by `cfg(debug_assertions)` or `FOSTER_TEST_MODE=1`; do not widen this
- **Session keys are random UUIDs** — do not add any endpoint that accepts a user-supplied session ID without validation
- **Schema validation runs on both `send()` and `restore()`** — do not add a path that bypasses it
