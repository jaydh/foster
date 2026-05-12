# Foster

A Rust web UI framework where UI is a pure function of server-managed state.
Designed so an LLM can write, test, and iterate on a full-stack app with no
hidden state, no framework magic, and a Playwright suite generated automatically
from the machine definition.

See [`CLAUDE.md`](CLAUDE.md) for the full reference (architecture, API, DSL, deployment).

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
│  State lives here: HashMap<(session_id, machine_id),    │
│                            MachineInstance>             │
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
│       fx-show      → toggle display                     │
│       fx-text      → set textContent                    │
│       fx-class     → add/remove CSS class               │
│       fx-on        → addEventListener → POST /trans.    │
│       fx-for       → clone template per item            │
│       fx-collect   → read input into payload            │
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
The WASM client subscribes to SSE *before* the initial fetch, so test-injected
state is never missed.

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
aura.spec.ts  (23 tests for 4 states, 12 edges)
```

No test IDs needed. `[fx-on="click->EVENT"]` is the universal locator;
`data-fx-state` is the universal assertion target. Both come from the framework.

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
