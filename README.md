# Foster

A Rust web UI framework where UI is a pure function of state-machine state.
Every piece of UI state is a named state machine; the page is HTML annotated
with `fx-*` attributes, and a generic WASM client renders it — no
app-specific JavaScript.

Machines come in three kinds:

- **Server machines** (default) — one instance per visitor session, owned by
  the server; events go over HTTP and updates come back over SSE.
- **Shared machines** (`.shared()`) — one server-side instance for every
  visitor, optionally driven by server-side data (`Foster::feed`): live
  dashboards, background-job status.
- **Local machines** (`.local()` / `.persist()`) — run entirely in the
  browser for per-visitor UI state (theme, dropdowns, lightboxes); no
  network at all.

For what markup can't express (a WebGL simulation), `fx-widget` lazy-loads
an app module — typically another Rust crate compiled to WASM — when it
scrolls into view.

---

## How it works

### Components

```
┌─────────────────────────────────────────────────────────┐
│                    Machine Definition                    │
│  MachineBuilder::new("aura", "calm", ctx)               │
│    .on("calm", "focus", "focused", reducer)             │
│    .template(include_str!("index.html"))                │
│    [.shared() | .local() | .persist()]                  │
│    .build()  →  Arc<Machine>                            │
│                                                         │
│  Foster::new(machines)                                  │
│    .feed("cluster", "tick", stream)    // shared + feed  │
│    .request_event("trace", "trace", f) // per-request    │
│    .router()                                            │
└────────────┬────────────────────────────────────────────┘
             │ shared (Arc) across all handlers
             ▼
┌─────────────────────────────────────────────────────────┐
│                   foster-server (Axum)                  │
│                                                         │
│  GET  /                  → template HTML (+ local machine│
│                            definitions as JSON)         │
│  GET  /state             → MessagePack snapshot          │
│  POST /transition        → run reducer → new snapshot    │
│  GET  /events?subs=…     → one SSE stream for all of a  │
│                            page's machines: "snapshot" + │
│                            "patch", tagged with "sub"    │
│  POST /test/state        → inject snapshot (debug only)  │
│  GET  /debug/history     → Vec<Snapshot> (debug only)   │
│  POST /debug/rewind      → restore snapshot (debug only) │
│  GET  /debug/graph       → SVG state graph (debug only) │
│                                                         │
│  State: (session_id, machine_id) → MachineInstance      │
│         (shared machines use one session for everyone)
└────────────┬────────────────┬───────────────────────────┘
    HTTP/MsgPack           SSE push
             │                │
             ▼                ▼
┌─────────────────────────────────────────────────────────┐
│              foster-client (WASM, runs in browser)      │
│                                                         │
│  1. Session ID (URL → localStorage → new random UUID)   │
│  2. Local machines: run in-process (localStorage if     │
│     .persist()); no network                             │
│  3. Server machines: one SSE stream for all of them,    │
│     then GET /state each → apply_snapshot_if_newer()    │
│  4. Walk DOM, wire fx-* attributes:                     │
│       fx-show / fx-if   → toggle display                │
│       fx-text / fx-field → set textContent              │
│       fx-class / fx-bind-attr → classes, attrs, styles  │
│       fx-on     → DOM event / visible / enter /         │
│                   click@outside → send event            │
│       fx-for    → clone template per item (skipped when │
│                   the list is unchanged)                │
│       fx-widget → lazy import(), mount(el), update(…)   │
│  5. On SSE snapshot/patch → apply_snapshot_if_newer()   │
└─────────────────────────────────────────────────────────┘
```

### What happens when you click a button

```
Browser                    foster-client (WASM)         foster-server
   │                              │                           │
   │  click [fx-on="click->focus"]│                           │
   │─────────────────────────────►│                           │
   │                              │  POST /transition         │
   │                              │  { machine, event,        │
   │                              │    session, payload }     │
   │                              │──────────────────────────►│
   │                              │                           │ machine.send("focus")
   │                              │                           │ → reducer(ctx, payload)
   │                              │                           │ → Snapshot { state,
   │                              │                           │     context, version }
   │                              │◄──────────────────────────│ Snapshot (msgpack)
   │                              │◄──────────────────────────│ SSE broadcast
   │                              │                           │
   │                              │  SSE "patch" event        │
   │                              │  apply patch to ctx cache │
   │                              │  apply_snapshot_if_newer()│
   │                              │  data-fx-state, fx-class, │
   │                              │  fx-text all update       │
   │◄─────────────────────────────│                           │
   │  DOM updated (no reload)     │                           │
```

Version ordering prevents out-of-order SSE messages from rolling state backwards.
For a local machine the same click never leaves the browser: the client runs
the transition itself and re-renders. The full attribute and API reference is
in [`CLAUDE.md`](CLAUDE.md).

### Test generation

```
Machine definition (gen_tests.rs)
  │
  │  transitions() → [(from, event, to), ...]
  │  state_names() → ["calm", "energized", ...]
  │
  ▼
foster_testgen::generate()
  │
  ├── Transition coverage  (1 test per directed edge)
  │     inject source → click [fx-on="click->EVENT"] → assert data-fx-state
  │
  ├── Multi-step walk  (1 test)
  │     greedy walk visiting every state ≥2× in sequence
  │     catches SSE ordering bugs and stale data-fx-state
  │
  ├── Rapid toggle pairs  (1 test per bidirectional pair)
  │     ping-pong 4× per pair — catches fx-class sync bugs
  │
  └── Snapshot injection  (1 test per state)
        POST /test/state → assert data-fx-state
  │
  ▼
aura.spec.ts  (23 tests for 4 states, 12 edges — nothing written by hand)
```

---

## Quick start

```bash
# Build WASM (dev) + start all demos — one script does everything
./scripts/demo.sh
#   http://localhost:3000  counter
#   http://localhost:3001  player
#   http://localhost:3002  kanban
#   http://localhost:3003  aura
#   http://localhost:3004  checkout

# Run tests for one example
cd examples/aura && npx playwright test
```

## Examples

| Example | Port | What it demonstrates |
|---------|------|----------------------|
| `counter`  | 3000 | Minimal idle/error machine, increment/decrement |
| `player`   | 3001 | 6-state media player, `fx-show` per state |
| `kanban`   | 3002 | Multi-column board, `fx-for` list rendering |
| `aura`     | 3003 | CSS animation showcase, `fx-class` state highlighting |
| `checkout` | 3004 | 7-state checkout flow; showcases graph, history, and dev overlay |

---

## Design decisions

**Why state machines over free-form atoms?**
Named states with typed transitions give an exhaustive, derivable state space. The test generator knows every valid event from every state, so it can cover the full graph by construction. Free-form reducers lose this.

**Why HTML-first over proc-macro RSX?**
HTMX lesson: behavior expressed as attributes is directly inspectable in devtools. No build-step mental model. Non-Rust contributors can edit templates.

**Why MessagePack?**
Binary, compact, schema-preserving. `rmp-serde` serializes the same `Snapshot` struct the server uses internally — no translation layer. JSON stays for `/test/state` because that endpoint needs to be curl-friendly.

**Why SSE over WebSockets?**
SSE is unidirectional, text-based, and handled natively by `EventSource` — no protocol upgrade or reconnect logic. The push direction (server → client) is all Foster needs; transitions go over the existing REST endpoints. The SSE stream uses named events: `snapshot` (full state) on first connection, then `patch` (RFC 6902 JSON Patch of the context) for subsequent transitions — keeping wire payload small for large context objects.

**Why three kinds of machines?**
"Server owns all state" is right for real data and wrong for a theme toggle: a
per-session server instance means a round trip per click, and a global one would
flip dark mode for everyone. Local machines keep per-visitor UI state in the
browser with the same markup and the same machine definitions (limited to
declarative transitions — `.pass()`, `.merge()`, `.step()` — since the generic
client can't run app Rust). Shared machines cover data that really is global,
and feeds let the server drive them, polling only while someone is watching.

**Why widgets instead of more attributes?**
Some UI is a simulation, not a function of state (WebGL running every frame).
Rather than grow the attribute language or fall back to page scripts,
`fx-widget` loads an app-provided module (usually a Rust crate built with
`wasm-pack --target web`) only when it nears the viewport, and passes it the
surrounding machine's snapshots via `update()`.

**Why inline schema validation?**
`foster-core` compiles to both native and `wasm32-unknown-unknown`. External JSON Schema crates pull in filesystem/network deps that don't compile to WASM. The inline validator covers the subset needed for context shape enforcement with zero dependencies.

---

## Deployment

Foster's app server is plain HTTP/1.1. **HTTP/2 termination belongs at the edge.** The
client already multiplexes every machine on a page onto a single SSE stream (one
stream per machine exhausted the browser's six-connection-per-origin HTTP/1.1
limit and starved `/transition`), but a long-lived SSE connection still holds one
of those six slots per tab.

**Version the WASM bundle URLs.** `foster_client.js` and `foster_client_bg.wasm`
must always come from the same build — a stale cached glue file next to a new
`.wasm` makes instantiation fail with a `LinkError` and nothing on the page works.
CDNs and browsers cache them on different schedules, so serve the page uncached
and reference the bundle with a content-hash query string, passing the `.wasm` URL
explicitly:

```js
const init = (await import('/pkg/foster_client.js?v=HASH')).default;
init({ module_or_path: '/pkg/foster_client_bg.wasm?v=HASH' });
```

```bash
# Local: Caddy provisions a trusted TLS cert automatically
brew install caddy
caddy reverse-proxy --from https://localhost:3000 --to localhost:3000
```

Production: any HTTP/2-capable proxy works — Caddy, nginx, Envoy, Cloudflare.

---

## Roadmap

### Implemented
- **Time-travel debugger** — `GET /debug/history` (50-entry ring buffer), `POST /debug/rewind?version=N`; gated by `FOSTER_TEST_MODE=1`
- **State graph UI / timeline** — `GET /debug/graph`, `GET /debug/timeline`; gated by `FOSTER_TEST_MODE=1`
- **Dev overlay** — Rust/WASM floating panel (debug builds only)
- **Differential rendering** — SSE `snapshot` + RFC 6902 `patch` events
- **Multiple machines per page** — `fx-machine="counter#1"` instance addressing
- **Generated TypeScript SDK** and **compiled machine validation** (`machine_graph!`)
- **High availability** — `StateStore` / `PubSub` traits with Redis impls
- **Local machines** — `.local()` / `.persist()`, declarative `.merge()` / `.step()` transitions
- **Shared machines** — `.shared()`, `Foster::feed` (polled only while watched), `Foster::request_event`
- **Multiplexed SSE** — one `/events?subs=…` stream per page
- **Widgets** — `fx-widget` lazy-loaded app modules with `mount` / `update`
- **More bindings and triggers** — `fx-bind-attr` `item:` / `style.*`, `fx-on` `visible` / `enter` / `click@outside`, cross-machine `->machine:event`

See [`CLAUDE.md`](CLAUDE.md) for the complete, current list.
