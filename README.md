# Foster

A Rust web UI framework: the state lives in named state machines, the page
is HTML annotated with `fx-*` attributes, and one generic WASM client renders
it — no app-specific JavaScript, no front-end build.

- **Server machines** (default) — one instance per browser tab, owned by the
  server; events go over HTTP, changes come back over SSE as JSON patches.
- **Shared machines** (`.shared()`) — one instance for everyone, driven by
  server-side data (`Foster::feed`): live dashboards, job status.
- **Local machines** (`.local()` / `.persist()`) — run in the browser for
  per-visitor UI state (a theme, a lightbox); no network.

What a page only *looks at* — a list's filter, sort, selection, keyboard
focus — is the browser's too (`fx-view`), with no round trip. For what markup
can't express (a WebGL simulation), `fx-widget` lazy-loads an app module.

Used by [files](https://github.com/jaydh/files) and
[jaydanhoward.com](https://github.com/jaydh/jaydanhoward). The working
reference for attributes and APIs is [`CLAUDE.md`](CLAUDE.md).

## How it works

```
 browser                                   server (axum)
┌──────────────────────────────┐          ┌──────────────────────────────┐
│ foster-client (WASM)         │ POST     │ /transition                  │
│  fx-on: DOM event → event ───┼─────────▶│  reducer(ctx, payload) → ctx │
│  apply patch (base→version) ◀┼──────────┤  ↳ reply: JSON patch         │
│                              │          │  ↳ effect (async I/O) → event│
│  SSE /events: snapshot, then ◀┼──────────┤  publish → every open page   │
│  patches; reconnects itself  │          │                              │
│  fx-* bindings → DOM         │          │ store: one snapshot per      │
│  fx-view: filter/sort/select │          │ (session, machine); idle     │
│  fx-url: field ↔ URL hash    │          │ sessions expire              │
└──────────────────────────────┘          └──────────────────────────────┘
```

A click posts the event; the reducer (plain Rust, `Value → Value`) makes the
new context; the response and the SSE stream carry a JSON patch from the
version the click started from, and the client applies it only onto that
version (a gap refetches). Effects do the I/O after a transition and report
back with more events.

## Quick start

```bash
./scripts/build-wasm.sh          # foster-client → pkg/
cargo run -p counter             # http://localhost:3000
(cd examples/counter && npm install && npx playwright test)
./scripts/check.sh               # cargo test + clippy
```

## Design decisions

**State machines, not free-form state.** Named states and transitions make
what can happen explicit; reducers are pure, so the server can retry a
transition whose write raced another instead of dropping it.

**HTML-first, not a component DSL.** Behaviour as attributes is inspectable
in devtools and needs no build step.

**Patches over SSE.** Server → client is all the push Foster needs; SSE is
native and multiplexes every machine on a page onto one stream. Contexts can
be large (a folder of 2,000 files), so both channels send RFC 6902 patches
between versions, never whole contexts after the first.

**The browser owns what the page looks at.** Server state for a filter box
or a checkbox is a round trip per keystroke; `fx-view` keeps a list's view
state per tab and hands the server the selection when an action needs it.

**Sessions per tab, bounded memory.** Each tab claims a session slot (Web
Locks), so tabs don't drive each other and a closed tab's state is reused.
The server keeps one snapshot per instance and drops idle, unwatched ones.

## Deployment

The app server is plain HTTP/1.1; terminate HTTP/2 at the edge (one SSE
stream per tab still holds one of HTTP/1.1's six connections per origin).

Serve `foster_client.js` and `foster_client_bg.wasm` from the same build —
a stale cached glue file next to a new `.wasm` fails to instantiate. Version
the URLs:

```js
const init = (await import('/pkg/foster_client.js?v=HASH')).default;
init({ module_or_path: '/pkg/foster_client_bg.wasm?v=HASH' });
```

Shut down promptly: open `/events` streams never end on their own, so give
in-flight requests a few seconds after SIGTERM and exit rather than waiting
for every connection (the client reconnects and resyncs).
