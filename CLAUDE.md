# Foster — guide for working on it

Server-owned state machines with a generic WASM client. A page is HTML with
`fx-*` attributes; the client wires them to machines on the server (or, for
`.local()` machines, in the browser) and keeps the DOM in sync over SSE.
What a page *looks at* — a list's filter, sort, selection, focus — stays in
the browser (`fx-view`). Users: files (`~/repos/files`) and jaydanhoward.com
(`~/repos/jaydanhoward`), each pinning a Foster rev in its Cargo.toml.

## Layout

```
crates/
  foster-core/     state machines (MachineBuilder, MachineInstance, Snapshot), fx-on grammar, page()
  foster-server/   axum router: /state, /transition, /events (SSE), effects, feeds, identity, sessions
  foster-client/   WASM runtime for fx-* attributes (wasm-only; not in the workspace)
    src/view.rs    list views (fx-view)
    src/url.rs     fx-url, per-tab sessions
examples/counter/  smallest app + browser smoke test
scripts/           build-wasm.sh, check.sh
```

## Loop

```bash
./scripts/check.sh                       # cargo test + clippy (native crates)
./scripts/build-wasm.sh                  # foster-client → pkg/
cargo run -p counter                     # http://localhost:3000
(cd examples/counter && npx playwright test)
```

Real coverage lives in the apps: files' `e2e/` (Playwright, ~20 tests) runs
against a local Foster with
`cargo build --config 'patch."https://github.com/jaydh/foster".foster-server.path="…/crates/foster-server"'`
(and the same for foster-core) and `FILES_PKG=~/repos/foster/pkg`.

## Machines (foster-core)

```rust
MachineBuilder::new("counter", "idle", json!({ "count": 0 }))
    .state("error")
    .on("idle", "increment", "idle", |ctx, _payload| Ok(json!({ "count": ctx["count"].as_i64().unwrap_or(0) + 1 })))
    .pass("idle", "break_it", "error")       // context unchanged
    .template(page("Counter", css, body))     // the page, served at GET /
    .build()
```

| Method | |
|---|---|
| `.on(from, event, to, reducer)` | `fn(Value ctx, Value payload) -> Result<Value, MachineError>`; pure, synchronous |
| `.pass(from, event, to)` | no change to context |
| `.merge(from, event, to)` | shallow-merge the payload into context |
| `.step(from, event, to, list, index, by)` | move `ctx[index]` through `ctx[list]`, merge the item (prev/next) |
| `.local()` / `.persist()` | runs in the browser (only pass/merge/step); `.persist()` saves to localStorage |
| `.shared()` | one instance for every visitor (feeds drive it) |

`Snapshot { machine_id, state, context: Arc<Value>, version, last_event }` —
the context is shared, so storing/broadcasting a snapshot copies a pointer;
the reducer gets the one deep copy (a failing reducer leaves the instance
intact). Build one with `json!(…).into()`; read with `snap.context["x"]`.

## Server (foster-server)

```rust
Foster::new(machines)
    .identity(|req| /* user from a trusted proxy header, or None → 401 */)
    .effect("dir", "open", |fx: Effect| async move { /* I/O, then fx.send("loaded", …) */ })
    .feed("cluster", "tick", stream)                 // .shared() machines; polled only while watched
    .request_event("trace", "trace", |req| async { … })
    .session_ttl(Some(Duration::from_secs(6 * 3600)))   // default; None keeps sessions forever
    .router()
```

- **Effects** run after a committed transition and report back with
  `fx.send(event, payload)` / `fx.send_to(machine, …)`; one per (machine,
  event). They see `fx.snapshot` (after the transition), `fx.payload`,
  `fx.identity`.
- **Concurrency**: a write that raced another to the same instance is
  retried (reducers are pure) — two effects reporting at once both land.
- **Memory**: one snapshot per instance; no history unless debug mode.
  Instances idle for `session_ttl` with no page subscribed are dropped.
- **Debug mode** (debug builds, or `FOSTER_TEST_MODE=1`): `POST /test/state`
  (inject a snapshot) and `GET /debug/history` (last 50 snapshots). Off in
  release.

| Route | |
|---|---|
| `GET /state?machine=&session=` | current snapshot (msgpack) |
| `POST /transition` | `{machine, event, payload, session, reply}` → snapshot, or with `reply: "patch"` (what the client sends) a JSON Patch `{base, version, patch}` |
| `GET /events?subs=m@s,…` | SSE: a full `snapshot` per sub on connect, then `patch`es (each with `base`). A request-event machine with no instance on this process gets none (its page's came from `/state`, maybe on another replica) |

## Client (foster-client)

**Sync.** Each instance's last snapshot is cached with its version. Patches
from either channel (a click's response, SSE) apply only onto their `base`;
already-seen ones are skipped; a gap refetches `/state`. A reconnect's full
snapshot is always taken (the server may have dropped the session or
restarted). The client reopens `/events` itself if the browser gives up
(back-off 1 s → 30 s).

**Sessions.** One per tab: a per-browser base (localStorage) plus the tab's
Web Locks slot (`<base>-t0`, `-t1`…; a closed tab's slot is reused).
`?session=` overrides; `<meta name="foster-session" content="browser">`
shares one across tabs.

**Rendering.** Lists first (so `$view` values are current), then show/if/
text/value/class/bind-attr. `fx-for` updates only changed rows (by
`fx-key`); rows bind to their item only.

### Attributes

| Attribute | Example | |
|---|---|---|
| `fx-machine` | `fx-machine="counter"` | root; `"counter#2"` for a second instance. Gets `data-fx-state/version/session` |
| `fx-show` | `fx-show="idle,loading"` | shown only in these states (`display:block`) |
| `fx-if` | `fx-if="error"` / `fx-if='{"field":"n","op":"gt","value":0}'` | shown when truthy / the comparison holds (`eq neq gt lt gte lte`) |
| `fx-text` | `fx-text="count"` | text from context |
| `fx-value` | `fx-value="draft"` | fill an input (only when the value or state changed) |
| `fx-class` | `fx-class="open:is-open"` | class in a state |
| `fx-bind-attr` | `fx-bind-attr="href=ctx:url hidden=item:no_link style.left=ctx:x disabled=state:busy"` | attributes from context / row item / state |
| `fx-for` / `fx-key` / `fx-field` | `fx-for="tasks" fx-key="id"` … `fx-field="title"` | a row per item, keyed; text from the item |
| `fx-on` | `fx-on="click->save keydown.Enter->save click.shift->range"` | DOM event → machine event; also `visible->`, `enter->`, `click@outside->`, `keydown.Escape@document->`, `->other:event` |
| `fx-collect` | `fx-collect="title"` | input value into payloads (every event of the machine; the firing field wins) |
| `fx-payload` | `fx-payload='{"col":"done"}'` | static JSON merged into the payload (wins over everything) |
| `fx-copy` | `fx-copy="item:url"` | copy to the clipboard on click |
| `fx-widget` | `fx-widget="/widgets/x.js"` | lazy ES module: `mount(el)`, `update(el, state, ctxJson)` |
| `fx-url` | `fx-url="path->open"` | one root: context field ↔ URL hash; Back/Forward and links fire the event; on (re)connect the URL wins |
| `fx-view*` | see below | a list's filter/sort/selection/focus in the browser |

Payload order: collected inputs, then the row's item (`data-fx-item`), then
a list view's `selected`/`focused` or a step's neighbour, then `fx-payload`.
To send something computed in page JS, set `fx-payload` on a (hidden)
button and click it.

### List views (`fx-view`)

| Attribute | Where | |
|---|---|---|
| `fx-view="files"` | `fx-for` container (with `fx-key`) | rows filtered/sorted; `selected`/`focused` classes. Lists sharing a name share the view |
| `fx-view-filter-fields` / `fx-view-group` / `fx-view-reset` | container | fields the filter searches (default `name`) / truthy-first field / context fields whose change resets filter, selection, focus |
| `fx-view-filter="files"` | input | every word must be in one of the fields |
| `fx-view-sort="files:field"` | button | numbers numerically, text case-insensitively; again flips |
| `fx-view-select` / `-select-all` / `-clear` | row / anywhere | shift-click selects a range |
| `fx-view-keys="files"` | focusable element | ↑/↓ move focus |
| `fx-view-payload="files"` | ancestor of `fx-on` elements | events carry `selected` (keys) and `focused` (item) |
| `fx-view-step="files:1"` + `fx-view-from` (+ `fx-view-where`) | button | disabled without a neighbour; its event carries the neighbour's item. `fx-view-pos` shows "3 of 12" |
| `fx-view-inputs="files" data-name="path"` | in a `<form>` | hidden inputs for the selection |
| `$files.shown/total/selected/all_selected/filtered/filter/sort_<field>` | binding keys | the view's values |

State is per tab (sessionStorage). Full reference: `crates/foster-client/src/view.rs`.

## Template validation

`.template(html)` checks each machine's own `[fx-machine]` subtree at
startup: an `fx-show` state or `fx-on` event the machine doesn't have
panics.

## Security invariants — don't break

- State changes only through transitions (`machine.send()` via `fire()`).
- `POST /test/state` and `/debug/history` only in debug mode.
- With `identity`, every per-session store key goes through
  `AppState::session(machine, requested, who)` — a client-supplied session ID
  is never a key on its own, so users can't address each other's sessions.
- Session IDs from the client are just names within the user's own space;
  they're not authentication.
