# How Foster Works

Three diagrams: architecture, runtime request cycle, and test generation.

---

## Architecture

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
│       …                                                 │
│  5. On SSE snapshot → apply_snapshot_if_newer()         │
└─────────────────────────────────────────────────────────┘
```

---

## Runtime: what happens when you click a button

```
Browser                    foster-client (WASM)         foster-server
   │                              │                           │
   │  click [fx-on="click->focus"]│                           │
   │─────────────────────────────►│                           │
   │                              │  POST /transition         │
   │                              │  { machine:"aura",        │
   │                              │    event:"focus",         │
   │                              │    session:"abc123",      │
   │                              │    payload:{} }  (msgpack)│
   │                              │──────────────────────────►│
   │                              │                           │ machine.send("focus")
   │                              │                           │ → reducer(ctx, payload)
   │                              │                           │ → new Snapshot {
   │                              │                           │     state:"focused",
   │                              │                           │     context:{...},
   │                              │                           │     version: N+1 }
   │                              │  Snapshot (msgpack) ◄─────│
   │                              │◄──────────────────────────│
   │                              │                           │ broadcast to SSE subscribers
   │                              │  SSE: snapshot ◄──────────│ (same session)
   │                              │◄──────────────────────────│
   │                              │                           │
   │                              │ apply_snapshot_if_newer() │
   │                              │  data-fx-state="focused"  │
   │                              │  fx-class buttons sync    │
   │                              │  fx-text updates          │
   │◄─────────────────────────────│                           │
   │  DOM updated (no reload)     │                           │
```

**Version ordering:** `apply_snapshot_if_newer` drops any snapshot with `version ≤ current`,
so out-of-order SSE messages can't roll state backwards. `restore()` (test injection) always
increments version, so injected state always wins over any in-flight response.

---

## Testgen: machine definition → full Playwright suite

```
Machine definition (gen_tests.rs)
  │
  │  transitions() → [(from, event, to), ...]
  │  state_names() → ["calm", "energized", ...]
  │
  ▼
foster_testgen::generate()
  │
  ├── injectState helper (with WASM bootstrap wait baked in)
  │
  ├── Transition coverage  (1 test per directed edge)
  │     inject source state → click [fx-on="click->EVENT"] → assert data-fx-state
  │
  ├── Multi-step walk  (1 test, greedy graph traversal)
  │     at each step: pick transition to least-visited state
  │     stops when every state visited ≥ 2×
  │     catches: SSE ordering bugs, stale data-fx-state
  │
  ├── Rapid toggle pairs  (1 test per bidirectional pair)
  │     find all (A→B, B→A) pairs (excluding self-loops)
  │     ping-pong 4× each, assert state each step
  │     catches: fx-class / animation sync bugs
  │
  └── Snapshot injection  (1 test per state)
        POST /test/state → assert data-fx-state (sanity baseline)
  │
  ▼
aura.spec.ts  (23 tests for a 4-state fully-connected graph)
  12 edge  +  1 walk  +  6 toggle pairs  +  4 injection
```

**Why no test IDs needed:** `[fx-on="click->EVENT"]` is the universal button locator and
`data-fx-state` on the machine root is the universal assertion target. Both come from the
framework, so testgen produces valid locators and assertions without reading the HTML.
