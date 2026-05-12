# Foster

A Rust-based web UI framework designed for LLM-assisted development workflows.

## Core philosophy

The framework's primary "user" is an LLM writing and debugging code.  Humans review passing tests and state diffs, not raw code.  This shapes every design decision:

- **UI is a pure function of serializable server-managed state** — no implicit client-side state, no hidden component lifecycle
- **State machines as the contract** — named states with typed transitions give the LLM an exhaustive, derivable state space; free-form reducers would lose this
- **HTML-first attribute DSL** — HTMX-style attributes keep behavior inspectable in browser devtools without a build-step mental model
- **Testing is a first-class primitive** — `POST /test/state` lets Playwright inject arbitrary state without replaying interactions; `data-fx-state` on the machine root gives a universal assertion target

## Architecture

```
foster/
├── crates/
│   ├── foster-core/       # State machine primitives & serialization
│   ├── foster-server/     # Axum HTTP router (3 routes)
│   ├── foster-client/     # WASM runtime — processes fx-* attributes
│   └── foster-testgen/    # Playwright test generation from machine definition
└── examples/
    └── counter/           # Runnable PoC: idle ↔ error counter machine
        ├── src/
        │   ├── main.rs          # Server + machine definition
        │   └── bin/gen_tests.rs # Generates counter.spec.ts
        ├── static/index.html    # HTML template with fx-* attributes
        ├── tests/               # Generated Playwright tests (committed)
        └── playwright.config.ts # Generated Playwright config
```

### Workspace layout

The workspace (`Cargo.toml`) includes the native crates.  `crates/foster-client` is **excluded** because it targets `wasm32-unknown-unknown` and must be built separately with `wasm-pack`.

## Running the PoC

```bash
# 1. Build the WASM client (one-time, re-run after editing foster-client)
cd crates/foster-client
wasm-pack build --target web --out-dir ../../examples/counter/pkg
cd ../..

# 2. Start the server
cargo run -p counter
# → http://localhost:3000
```

## State machine (foster-core)

```rust
MachineBuilder::new("counter", "idle", json!({ "count": 0 }))
    .state("error")
    .on("idle", "increment", "idle", Some(increment_fn))
    .on("idle", "break_it",  "error", Some(passthrough))
    .on("error", "recover",  "idle",  Some(passthrough))
    .build()  // → Arc<Machine>
```

- `Machine` is static and `Arc`-shared (Send + Sync via `fn` pointer reducers, no closures)
- `MachineInstance` is mutable runtime state: current state + context `serde_json::Value` + monotonic version
- `Snapshot` is the unit of everything: wire format, time-travel, test injection, state diffing

Key invariant: **state transitions are the only way state changes**.  The server owns instances; the client is a render layer.

## HTTP API (foster-server)

| Method | Path | Format | Purpose |
|--------|------|--------|---------|
| GET | `/state?machine=<id>` | MessagePack | Current snapshot |
| POST | `/transition` | MessagePack in/out | Fire event, get new snapshot |
| POST | `/test/state` | JSON in/out | Inject arbitrary snapshot (Playwright/curl) |

`/test/state` intentionally stays JSON — it must be trivially curl-able during debugging without a msgpack encoder.

```bash
# Force the machine into error state
curl -X POST http://localhost:3000/test/state \
     -H 'Content-Type: application/json' \
     -d '{"machine_id":"counter","state":"error","context":{"count":99},"version":42}'
```

## HTML template DSL (fx-* attributes)

All attributes are processed client-side by the WASM runtime after fetching a snapshot.

| Attribute | Example | Meaning |
|-----------|---------|---------|
| `fx-machine` | `fx-machine="counter"` | Machine root; stamped with `data-fx-state` / `data-fx-version` after each transition |
| `fx-show` | `fx-show="idle,loading"` | Visible only in listed states (comma-separated) |
| `fx-text` | `fx-text="count"` | Set text from `context[key]` |
| `fx-disable` | `fx-disable="loading"` | Add `disabled` attribute in listed states |
| `fx-on` | `fx-on="click->increment"` | Fire machine event on DOM event |
| `fx-state-label` | `fx-state-label` | Display current state name |

CSS convention: `[fx-show] { display: none; }` hides all showable elements before WASM loads.

## Playwright integration

```typescript
const root = page.locator('[fx-machine="counter"]');

// Assert state
await expect(root).toHaveAttribute('data-fx-state', 'idle');

// Inject state without replaying interactions
await request.post('/test/state', {
  data: { machine_id: 'counter', state: 'error', context: { count: 99 }, version: 0 }
});
await page.reload();
await expect(root).toHaveAttribute('data-fx-state', 'error');

// Trigger a transition
await page.locator('[fx-on="click->recover"]').first().click();
await expect(root).toHaveAttribute('data-fx-state', 'idle');
```

### Test generation

`Machine::transitions()` returns all `(from, event, to)` triples.  `foster-testgen` generates one test per edge plus one injection-only test per state:

```bash
cargo run -p counter --bin gen_tests
# Writes: examples/counter/tests/counter.spec.ts
#         examples/counter/playwright.config.ts (only if absent)

cd examples/counter && npx playwright test
```

## Design decisions & rationale

**Why machine semantics over free-form atoms?**
Named states with typed transitions give an exhaustive, derivable state space.  The LLM knows all valid events from any state, so generated tests cover every edge by construction.  Free-form reducers lose this schema.

**Why HTML-first over proc-macro RSX?**
HTMX lesson: behavior expressed as attributes is directly inspectable in devtools.  No build-step mental model for the LLM.  Non-Rust contributors can edit templates.  Proc-macro RSX is valuable for human autocomplete, not for LLM generation.

**Why MessagePack for the wire protocol?**
Binary, compact, schema-preserving.  `rmp-serde` serializes the same `Snapshot` struct that the server uses internally — no translation layer.  JSON stays for `/test/state` because that endpoint must be curl-friendly.

**Why `fn` pointers for reducers, not closures?**
`fn` pointers are `Send + Sync`, making `Arc<Machine>` safe to share across Axum handlers without wrapping in `Arc<Mutex<...>>`.  Closures would require the machine definition to be inside a mutex.

**Why `closure.forget()` in the WASM client?**
Event listener closures are page-lifetime singletons.  Storing them in a registry adds complexity; leaking them is the conventional WASM-bindgen pattern for static listeners.

## What's next

### Near-term
- **Multi-session state**: key instances by `(session_id, machine_id)` rather than just `machine_id`; use HTTP session cookies or a URL param
- **SSE/push on state change**: after `POST /test/state`, push the new snapshot to all connected clients so Playwright doesn't need a `page.reload()`
- **Context schema enforcement**: validate `context` against a JSON Schema on state entry so the server rejects malformed injections
- **`fx-bind-attr`**: bind an HTML attribute to a context expression (`fx-bind-attr="href=ctx:url"`)

### Medium-term
- **Time-travel debugger**: store a ring buffer of snapshots server-side; expose `GET /history` and `POST /rewind?version=N`
- **Multiple machine instances on one page**: `fx-machine="counter#1"` / `fx-machine="counter#2"` instance addressing
- **Generated TypeScript SDK**: derive `setState(snapshot)` and `sendEvent(event, payload)` typed wrappers from the machine definition, so Playwright gets autocomplete

### Longer-term
- **Compiled state machine validation**: proc-macro that turns a state machine definition into a compile-time-checked type graph (Rust enum states, exhaustive match on events)
- **Differential rendering**: server sends a JSON Patch diff of context rather than the full snapshot, reducing wire payload for large context objects
- **Dev-mode overlay**: inject a floating panel showing current state, version, context, and recent transitions — queryable by Playwright as a test oracle
