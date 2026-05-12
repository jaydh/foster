# Foster

A Rust-based web UI framework designed for LLM-assisted development workflows.

## Core philosophy

The framework's primary "user" is an LLM writing and debugging code.  Humans review passing tests and state diffs, not raw code.  This shapes every design decision:

- **UI is a pure function of serializable server-managed state** — no implicit client-side state, no hidden component lifecycle
- **State machines as the contract** — named states with typed transitions give the LLM an exhaustive, derivable state space; free-form reducers would lose this
- **HTML-first attribute DSL** — HTMX-style attributes keep behavior inspectable in browser devtools without a build-step mental model
- **Testing is a first-class primitive** — `POST /test/state` lets Playwright inject arbitrary state without replaying interactions; `data-fx-state` on the machine root gives a universal assertion target

### Security as a structural property

Foster's architecture eliminates entire classes of attack by construction — not as hardening applied after the fact.

**All UI state lives on the server.** The client is a pure render layer: it receives a `Snapshot`, applies `fx-*` attributes to the DOM, and sends named events back. There is no client-side business logic to compromise, no hidden state that diverges from the server, and no way for a crafted payload to silently mutate the application because every state change goes through a named, server-validated transition.

**The state machine is the access control list.** Events that aren't declared for a given state are rejected outright — not by a middleware check, but by the machine's transition table. An attacker who sends a fabricated event gets a 400 from the same code path that rejects a typo from a developer. There is no separate "auth" layer to forget.

**Context schema validation closes the injection window.** Optional JSON Schema on each state (`.schema("state", json!({...}))`) is enforced on every state entry — both from normal transitions and from `POST /test/state` injection. A malformed context that would crash a reducer or leak data through a template can never reach the DOM.

**`POST /test/state` is compile-time gated.** In release builds the endpoint returns 403 unless `FOSTER_TEST_MODE=1` is explicitly set. This means the attack surface that exists during development disappears in production — not through documentation or convention, but through code.

**Session isolation is structural.** Every machine instance is keyed by `(session_id, machine_id)`. One user's session cannot read or write another's state because the key is unguessable (128-bit random UUID) and the lookup is by exact key — not by scan.

The result: an LLM generating Foster applications gets security for free, because the secure path is the only path the framework exposes.

## Architecture

```
foster/
├── crates/
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

### Workspace layout

`Cargo.toml` includes native crates.  `crates/foster-client` is **excluded** because it targets `wasm32-unknown-unknown` and must be built separately with `wasm-pack`.

## LLM iteration loop

Foster is designed so the LLM's primary feedback signal is a **failing Playwright assertion**, not a compiler error or a runtime exception. Every state name and event name in the test exactly matches the machine definition — there is nothing to invent.

### Tight loop (one command)

```bash
./scripts/check.sh          # cargo check + cargo test + gen_tests (all examples)
./scripts/check.sh kanban   # same, gen_tests for one example only
```

Run this after every change. It is fast (incremental type-check, no browser).

### Full loop

```bash
./scripts/demo.sh kanban    # start the server (separate terminal)
cd examples/kanban && npx playwright test --reporter=line
```

### Adding a feature

The minimal change for any new behavior:

1. Add a transition: `.on("from_state", "event_name", "to_state", reducer_fn)` or `.pass(...)` for no-op
2. Add the reducer: `fn reducer_fn(ctx: Value, payload: Value) -> Result<Value, MachineError>`
   — or use `.typed_on("from", "event", "to", reducer_fn)` with a typed struct
3. Add the HTML: `<button fx-on="click->event_name">label</button>`
4. Run `./scripts/check.sh` — tests for the new edge appear automatically (including walk and toggle-pair tests if applicable), template is validated

That's it. No test file to edit. No type file to update. The state machine is the single source of truth; everything else is derived.

### What good iteration looks like

```
LLM writes machine definition
  → ./scripts/check.sh passes (type-safe, unit tests green)
  → Playwright test for every edge exists (generated)
  → Run playwright test
  → One test fails: "kanban | viewing →[move_task]→ viewing"
  → The failing assertion tells you exactly which state/event/transition is wrong
  → Fix the reducer, re-run, green
```

The loop is tight because the failure is always in terms the LLM already knows — state names and event names from its own machine definition.

## Quick start

```bash
# 1. Build the WASM client (re-run after editing foster-client)
./scripts/build-wasm.sh

# 2. Start all demos
./scripts/demo.sh
#   http://localhost:3000  counter
#   http://localhost:3001  player
#   http://localhost:3002  kanban
#   http://localhost:3003  aura

# Or start one:
./scripts/demo.sh kanban
```

## State machine (foster-core)

```rust
MachineBuilder::new("counter", "idle", json!({ "count": 0 }))
    .state("error")
    // Optional: JSON Schema validated on every entry to this state
    .schema("idle", json!({
        "type": "object",
        "required": ["count"],
        "properties": { "count": { "type": "integer", "minimum": 0 } }
    }))
    .on("idle", "increment", "idle", |ctx, _| Ok(json!({ "count": ctx["count"].as_i64().unwrap_or(0) + 1 })))
    .on("idle", "reset",     "idle", |_, _|   Ok(json!({ "count": 0 })))
    .pass("idle",  "break_it", "error")   // passthrough — context unchanged
    .pass("error", "recover",  "idle")
    // Optional: co-locate HTML — served at GET /, validated at startup
    .template(include_str!("../static/index.html"))
    .build()  // → Arc<Machine>
```

### Transition methods

| Method | When to use |
|--------|-------------|
| `.on(from, event, to, reducer)` | Reducer transforms `Value` context directly |
| `.pass(from, event, to)` | Context passes through unchanged |
| `.typed_on(from, event, to, reducer)` | Reducer works with a typed struct — no json! unwrapping |

`.typed_on` example — field assignment replaces `json!` reconstruction:

```rust
#[derive(Serialize, Deserialize, Clone, Default)]
struct KanbanCtx { tasks: Vec<Task>, draft_title: String, editing_id: String, confirm_id: String }

fn begin_edit(mut ctx: KanbanCtx, payload: Value) -> Result<KanbanCtx, MachineError> {
    ctx.editing_id  = payload["id"].as_str().unwrap_or("").to_string();
    ctx.draft_title = ctx.tasks.iter().find(|t| t.id == ctx.editing_id)
                          .map(|t| t.title.clone()).unwrap_or_default();
    Ok(ctx)
}

builder.typed_on("viewing", "start_edit", "editing", begin_edit)
```

- `Machine` is static and `Arc`-shared; reducers are `Arc<dyn Fn>` so closures, fn pointers, and typed wrappers all work
- `MachineInstance` is mutable runtime state: current state + context `Value` + monotonic version
- `Snapshot` is the unit of everything: wire format, time-travel, test injection, state diffing

Key invariant: **state transitions are the only way state changes**.  The server owns instances; the client is a render layer.

### Template co-location and validation

`.template(html)` stores HTML in the machine. `foster_server::router()` then:
1. Serves it at `GET /` automatically — no explicit index handler needed
2. Validates every `fx-show` and `fx-on` attribute at startup — a typo panics the server immediately rather than silently doing nothing at runtime

```rust
// Panics at startup: "fx-on=\"click->incremnt\": event 'incremnt' not defined in machine 'counter'"
.template(r#"<button fx-on="click->incremnt">+</button>"#)
```

### Schema validation

Attach a JSON Schema to any state with `.schema(state, json_schema)`.  Supported keywords: `type`, `required`, `properties`, `minimum`, `maximum`, `minLength`, `maxLength`, `enum`.  Validation runs on both `send()` (reducer output) and `restore()` (test injection) — a schema violation returns `MachineError::SchemaViolation` before any state is committed.  No external dependencies; the validator is inlined and compiles to WASM.

## HTTP API (foster-server)

| Method | Path | Format | Purpose |
|--------|------|--------|---------|
| GET | `/state?machine=<id>&session=<sid>` | MessagePack | Current snapshot for session |
| POST | `/transition` | MessagePack in/out | Fire event, get new snapshot |
| GET | `/events?machine=<id>&session=<sid>` | SSE (JSON events) | Push stream — receives snapshots after any state change |
| POST | `/test/state?session=<sid>` | JSON in/out | Inject arbitrary snapshot (debug only — see below) |

`session` defaults to `"default"` if omitted.  `/test/state` stays JSON — trivially curl-able without a msgpack encoder.

```bash
# Inject error state into a specific session
curl -X POST 'http://localhost:3000/test/state?session=my-test' \
     -H 'Content-Type: application/json' \
     -d '{"machine_id":"counter","state":"error","context":{"count":99},"version":0}'
```

### Test endpoint security

`POST /test/state` is enabled **only** when:
- The binary is built in debug mode (`cargo run` / `cargo build`), OR
- `FOSTER_TEST_MODE=1` is set at runtime

In release builds without that env var, the endpoint returns `403 Forbidden`.  Set `FOSTER_TEST_MODE=1` if you need it in a staging environment.

### SSE push

After any `POST /transition` or `POST /test/state`, the server broadcasts the new `Snapshot` to all SSE subscribers for that `(session, machine)` pair.  The WASM client subscribes on load and applies incoming snapshots immediately — Playwright tests no longer need `page.reload()`.

## Session isolation

Every machine instance is keyed by `(session_id, machine_id)` and created lazily on first access.  Sessions never share state.

The WASM client reads `?session=<id>` from the URL.  If absent, it generates a 128-bit random ID for the tab.  The session ID is stamped as `data-fx-session` on `[fx-machine]` so Playwright can discover it:

```typescript
const sid = await root.getAttribute('data-fx-session');
await request.post(`/test/state?session=${sid}`, { data: { ... } });
```

## HTML template DSL (fx-* attributes)

All attributes are processed client-side by the WASM runtime after fetching a snapshot.

| Attribute | Example | Meaning |
|-----------|---------|---------|
| `fx-machine` | `fx-machine="counter"` | Machine root; stamped with `data-fx-state`, `data-fx-version`, `data-fx-session` |
| `fx-show` | `fx-show="idle,loading"` | Visible only in listed states (comma-separated) |
| `fx-text` | `fx-text="count"` | Set text from `context[key]` |
| `fx-disable` | `fx-disable="loading"` | Add `disabled` attribute in listed states |
| `fx-on` | `fx-on="click->increment"` | Fire machine event on DOM event |
| `fx-state-label` | `fx-state-label` | Display current state name |
| `fx-for` | `fx-for="tasks"` | Render one clone of the first child per item in `context[key]` |
| `fx-where` | `fx-where='{"column":"todo"}'` | Filter `fx-for` items by key/value match |
| `fx-field` | `fx-field="title"` | Set text from item field inside `fx-for` clone |
| `fx-collect` | `fx-collect="draft_title"` | Read input value into transition payload |
| `fx-value` | `fx-value="draft_title"` | Pre-populate input from context |
| `fx-payload` | `fx-payload='{"col":"done"}'` | Static JSON merged into transition payload |
| `fx-class` | `fx-class="calm:gentle"` | Add CSS class when in named state, remove otherwise |
| `fx-bind-attr` | `fx-bind-attr="href=ctx:url"` | Bind HTML attribute from context or state |

**`fx-bind-attr` format:** space-separated `attr=source:value` pairs.
- `attr=ctx:key` — set `attr` from `context[key]`; removes `attr` if key is absent
- `attr=state:statename` — set `attr=""` when in that state, remove otherwise (use for boolean attrs like `disabled`, `hidden`, `aria-current`)

CSS convention: `[fx-show] { display: none; }` hides all showable elements before WASM loads.

## Playwright integration

```typescript
const root = page.locator('[fx-machine="counter"]');

// Read session ID stamped by the WASM client
const sid = await root.getAttribute('data-fx-session');

// Assert state (SSE keeps this in sync — no reload needed)
await expect(root).toHaveAttribute('data-fx-state', 'idle');

// Inject state — SSE pushes it to the browser immediately
await request.post(`/test/state?session=${sid}`, {
  data: { machine_id: 'counter', state: 'error', context: { count: 99 }, version: 0 }
});
await expect(root).toHaveAttribute('data-fx-state', 'error');  // no page.reload()

// Trigger a transition
await page.locator('[fx-on="click->recover"]').first().click();
await expect(root).toHaveAttribute('data-fx-state', 'idle');
```

### Test generation

`foster-testgen` derives a complete Playwright suite from the machine graph — no manual test writing.  Four suites are generated automatically:

| Suite | What it covers |
|-------|----------------|
| **Transition coverage** | One test per directed edge: inject source state → click event → assert target |
| **Multi-step walk** | Greedy deterministic walk visiting every state ≥2× in sequence — catches SSE snapshot ordering and stale `data-fx-state` bugs that isolated tests miss |
| **Rapid toggle pairs** | Every pair of states connected in both directions ping-ponged 4× — catches `fx-class`/animation sync bugs |
| **Snapshot injection** | One test per state verifying `POST /test/state` works correctly |

When you add a transition, regenerating the spec adds tests for the new edge, the walk automatically extends to cover it, and a toggle-pair test appears if the new edge creates a bidirectional pair.  Nothing to write manually.

```bash
./scripts/gen-tests.sh          # all examples
./scripts/gen-tests.sh kanban   # one example
cargo run -p aura --bin gen_tests   # single example directly
```

The `injectState` helper in generated specs waits for WASM bootstrap before firing `POST /test/state`, so the SSE listener is always live before state is injected.

## State graph visualization (for human debugging)

Foster is LLM-first, but humans triaging failures need a way to understand what went wrong.  The plan:

**`GET /debug/graph`** — returns the machine's transition graph as a self-contained HTML page with an SVG rendered by D3 force layout.  Nodes are states; directed edges are events.  The current state of each session is highlighted in real-time via SSE.  Each node shows:
- State name
- Active session count
- Schema if defined
- Valid events from this state

**Dev overlay** — in debug builds, a small floating panel (bottom-right) shows the current state, version, last event, and a "jump to state" dropdown that calls `/test/state`.  Injected via a `<script>` tag in debug mode; absent in production.

**`GET /debug/history?session=<sid>&machine=<id>`** — returns the last N snapshots as a JSON array, enabling time-travel inspection without a full replay.

These are not yet implemented but are the designed interfaces.  A future PR adds them behind the `FOSTER_DEV_UI=1` env var.

## Design decisions & rationale

**Why machine semantics over free-form atoms?**
Named states with typed transitions give an exhaustive, derivable state space.  The LLM knows all valid events from any state, so generated tests cover every edge by construction.  Free-form reducers lose this schema.

**Why HTML-first over proc-macro RSX?**
HTMX lesson: behavior expressed as attributes is directly inspectable in devtools.  No build-step mental model for the LLM.  Non-Rust contributors can edit templates.  Proc-macro RSX is valuable for human autocomplete, not for LLM generation.

**Why MessagePack for the wire protocol?**
Binary, compact, schema-preserving.  `rmp-serde` serializes the same `Snapshot` struct that the server uses internally — no translation layer.  JSON stays for `/test/state` because that endpoint must be curl-friendly.

**Why `Arc<dyn Fn>` for reducers, not bare `fn` pointers?**
`Arc<dyn Fn(...) + Send + Sync>` allows non-capturing closures, named fn pointers, and typed reducer wrappers (from `.typed_on`) to all use the same storage.  The `Send + Sync` bounds on the trait object preserve thread safety — `Arc<Machine>` remains safe to clone across Axum handlers without a mutex.

**Why inline schema validation instead of a jsonschema crate?**
`foster-core` is shared between native server code and the WASM client.  External JSON Schema crates pull in network and filesystem dependencies that don't compile to `wasm32-unknown-unknown`.  The inline validator covers the subset that matters for context shape enforcement with zero dependencies.

**Why SSE instead of WebSockets for push?**
SSE is unidirectional, text-based, and handled natively by `EventSource` — no protocol upgrade, no frame parsing, no reconnect logic to write.  The push direction (server → client) is all Foster needs; transitions go over the existing REST endpoints.  Foster requires HTTP/2 at the edge (see Deployment below), so the historical per-domain connection limit on SSE does not apply.

**Why `closure.forget()` in the WASM client?**
Event listener closures and SSE `EventSource` handles are page-lifetime singletons.  Storing them in a registry adds complexity; leaking them is the conventional wasm-bindgen pattern for static handles.

## Comparative benchmarks

`benchmarks/` contains React implementations of the counter and kanban apps — the same specs, written idiomatically.  `scripts/measure.sh` reports LOC and token counts.

Foster costs fewer tokens to author in aggregate (counter: −365, kanban: −118 vs React including tests).  The meaningful differences are in `benchmarks/README.md`:

- **Test coverage**: React tests cover what the developer remembered. Foster tests cover every edge automatically — for kanban's 10 transitions, that's ~800 tokens the LLM never writes.
- **Template validation**: unknown `fx-show` states or `fx-on` events panic at server startup rather than silently doing nothing at runtime.
- **Feature delta**: adding a transition in Foster touches one `.on()` call, one reducer, one HTML attribute. React changes fan out across types, reducer, component(s), and test file.
- **Zero implicit bugs**: React's reconciler, hook deps, and stale closures are entire failure classes that simply don't exist in Foster.

## Deployment

Foster's app server is plain HTTP/1.1 — intentionally so.  **HTTP/2 termination belongs at the edge**, handled by a reverse proxy that the framework author chooses.  Foster requires HTTP/2 at that edge; running it behind HTTP/1.1 only will break SSE under load (six-connection-per-origin browser limit).

**Recommended local setup (Caddy):**

```bash
# Install: brew install caddy
caddy reverse-proxy --from https://localhost:3000 --to localhost:3000
```

Caddy automatically provisions a locally-trusted TLS cert via its built-in CA (no browser warnings, no manual cert steps) and negotiates h2 via ALPN.

**Playwright:** add `ignoreHTTPSErrors: true` to `playwright.config.ts` when testing against the local TLS endpoint, or point tests directly at the HTTP backend and accept HTTP/1.1 for test runs.

**Production:** put any HTTP/2-capable proxy in front — Caddy, nginx, Envoy, Cloudflare, etc.  The Foster app server itself has no opinion on TLS or the outer protocol.

## What's next

### Medium-term
- **Time-travel debugger**: ring buffer of snapshots server-side; `GET /debug/history` and `POST /rewind?version=N`
- **State graph UI**: `GET /debug/graph` — real-time SVG graph of all states with active session highlighting
- **Dev overlay**: floating panel in debug builds showing current state, version, last event, and jump-to-state dropdown
- **Multiple machine instances on one page**: `fx-machine="counter#1"` / `fx-machine="counter#2"` instance addressing
- **Generated TypeScript SDK**: derive `setState(snapshot)` and `sendEvent(event, payload)` typed wrappers from the machine definition

### Longer-term
- **Compiled state machine validation**: proc-macro that turns a state machine definition into a compile-time-checked type graph (Rust enum states, exhaustive match on events)
- **Differential rendering**: server sends a JSON Patch diff of context rather than the full snapshot, reducing wire payload for large context objects
- **CORS configuration**: `router(machines).cors(allow_origins)` — currently same-origin only; configurable allow-list for multi-origin deployments
