# Foster

A Rust web UI framework where UI is a pure function of server-managed state.
The server owns all state. The browser is a render layer. A WASM client
processes `fx-*` attributes and sends named events back over HTTP.

---

## How it works

### Components

```
┌─────────────────────────────────────────────────────────┐
│                    Machine Definition                    │
│  MachineBuilder::new("aura", "calm", ctx)               │
│    .on("calm", "focus", "focused", reducer)             │
│    .template(include_str!("index.html"))                │
│    .build()  →  Arc<Machine>                            │
└────────────┬────────────────────────────────────────────┘
             │ shared (Arc) across all handlers
             ▼
┌─────────────────────────────────────────────────────────┐
│                   foster-server (Axum)                  │
│                                                         │
│  GET  /            → serve template HTML                │
│  GET  /state       → MessagePack snapshot               │
│  POST /transition  → run reducer → new snapshot         │
│  GET  /events      → SSE stream of snapshots            │
│  POST /test/state  → inject snapshot (debug only)       │
│                                                         │
│  State: HashMap<(session_id, machine_id), MachineInstance>
└────────────┬────────────────┬───────────────────────────┘
    HTTP/MsgPack           SSE push
             │                │
             ▼                ▼
┌─────────────────────────────────────────────────────────┐
│              foster-client (WASM, runs in browser)      │
│                                                         │
│  1. Generate session ID (128-bit random UUID)           │
│  2. Subscribe SSE                                       │
│  3. GET /state → apply_snapshot_if_newer()              │
│  4. Walk DOM, wire fx-* attributes:                     │
│       fx-show   → toggle display                        │
│       fx-text   → set textContent                       │
│       fx-class  → add/remove CSS class                  │
│       fx-on     → addEventListener → POST /transition   │
│       fx-for    → clone template per item               │
│  5. On SSE snapshot → apply_snapshot_if_newer()         │
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
   │                              │  apply_snapshot_if_newer()│
   │                              │  data-fx-state, fx-class, │
   │                              │  fx-text all update       │
   │◄─────────────────────────────│                           │
   │  DOM updated (no reload)     │                           │
```

Version ordering prevents out-of-order SSE messages from rolling state backwards.

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
# Build the WASM client
./scripts/build-wasm.sh

# Start all demos
./scripts/demo.sh
#   http://localhost:3000  counter
#   http://localhost:3001  player
#   http://localhost:3002  kanban
#   http://localhost:3003  aura

# Run tests for one example
cd examples/aura && npx playwright test
```

## Examples

| Example | Port | What it demonstrates |
|---------|------|----------------------|
| `counter` | 3000 | Minimal idle/error machine, increment/decrement |
| `player`  | 3001 | 6-state media player, `fx-show` per state |
| `kanban`  | 3002 | Multi-column board, `fx-for` list rendering |
| `aura`    | 3003 | CSS animation showcase, `fx-class` state highlighting |

---

## Design decisions

**Why state machines over free-form atoms?**
Named states with typed transitions give an exhaustive, derivable state space. The test generator knows every valid event from every state, so it can cover the full graph by construction. Free-form reducers lose this.

**Why HTML-first over proc-macro RSX?**
HTMX lesson: behavior expressed as attributes is directly inspectable in devtools. No build-step mental model. Non-Rust contributors can edit templates.

**Why MessagePack?**
Binary, compact, schema-preserving. `rmp-serde` serializes the same `Snapshot` struct the server uses internally — no translation layer. JSON stays for `/test/state` because that endpoint needs to be curl-friendly.

**Why SSE over WebSockets?**
SSE is unidirectional, text-based, and handled natively by `EventSource` — no protocol upgrade or reconnect logic. The push direction (server → client) is all Foster needs; transitions go over the existing REST endpoints.

**Why inline schema validation?**
`foster-core` compiles to both native and `wasm32-unknown-unknown`. External JSON Schema crates pull in filesystem/network deps that don't compile to WASM. The inline validator covers the subset needed for context shape enforcement with zero dependencies.

---

## Deployment

Foster's app server is plain HTTP/1.1. **HTTP/2 termination belongs at the edge** — running behind HTTP/1.1 only will break SSE under load (six-connection-per-origin browser limit).

```bash
# Local: Caddy provisions a trusted TLS cert automatically
brew install caddy
caddy reverse-proxy --from https://localhost:3000 --to localhost:3000
```

Production: any HTTP/2-capable proxy works — Caddy, nginx, Envoy, Cloudflare.

---

## Roadmap

- **Time-travel debugger** — ring buffer of snapshots; `GET /debug/history`, `POST /rewind?version=N`
- **State graph UI** — `GET /debug/graph`, real-time SVG with active session highlighting
- **Dev overlay** — floating panel in debug builds: current state, version, last event, jump-to-state
- **Multiple machines per page** — `fx-machine="counter#1"` instance addressing
- **Generated TypeScript SDK** — typed `sendEvent` / `setState` wrappers derived from the machine
- **Compiled machine validation** — proc-macro turning the state graph into exhaustive Rust enums
- **Differential rendering** — JSON Patch diffs instead of full snapshots for large context objects
- **High availability / multi-replica** — `StateStore` + `PubSub` traits abstract the two in-memory bottlenecks; Redis impls make each replica stateless (see below)

### High availability design

Two in-memory structures block horizontal scaling today:

| Structure | Role | HA replacement |
|-----------|------|----------------|
| `Mutex<HashMap<(session, machine), MachineInstance>>` | State storage | External store (Redis, Postgres) |
| `Mutex<HashMap<(session, machine), broadcast::Sender>>` | SSE fan-out | Pub/sub (Redis pub/sub) |

`Snapshot` is already fully serializable (MessagePack), so both are straightforward to externalize.

**Proposed trait surface** (in `foster-server`):

```rust
#[async_trait]
pub trait StateStore: Send + Sync + 'static {
    /// Fetch the current snapshot, or `None` if this session hasn't started.
    async fn load(&self, session: &str, machine: &str) -> Option<Snapshot>;

    /// Persist a new snapshot. Must be atomic — use a version check (optimistic
    /// lock) so concurrent requests to the same session don't clobber each other.
    async fn store(&self, session: &str, machine: &str, snap: &Snapshot)
        -> Result<(), StoreError>;
}

#[async_trait]
pub trait PubSub: Send + Sync + 'static {
    /// Broadcast a snapshot to all SSE subscribers across all replicas.
    async fn publish(&self, session: &str, machine: &str, snap: Snapshot);

    /// Return a stream of snapshots for this (session, machine) pair.
    /// Each call returns a fresh subscription from the current moment.
    fn subscribe(&self, session: &str, machine: &str)
        -> impl Stream<Item = Snapshot> + Send + 'static;
}
```

`AppState` becomes generic: `AppState<S: StateStore, P: PubSub>`. The default `router()` call uses `InMemoryStore` + `InMemoryPubSub` (the current behavior). A Redis-backed router would pass `RedisStore` + `RedisPubSub`.

**Transition flow with external store:**

```
POST /transition
  → StateStore::load(session, machine)          // fetch current snapshot
  → Machine::apply(snapshot, event, payload)    // pure — no I/O
  → StateStore::store(session, machine, &next)  // atomic write (version check)
  → PubSub::publish(session, machine, next)     // fan-out to all replicas' SSE handlers
  → return next snapshot to caller
```

The version field already on `Snapshot` serves as the optimistic lock token — a Redis `WATCH`/`MULTI` or Lua CAS script rejects a write if the version changed since the load.
