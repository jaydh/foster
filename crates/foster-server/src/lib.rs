pub mod store;
pub use store::{InMemoryPubSub, InMemoryStore, PubSub, StateStore, StoreError};

use axum::{
    body::{Body, Bytes},
    extract::{Query, State},
    http::{header, StatusCode},
    response::{Html, sse::{Event, KeepAlive, Sse}, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use axum::extract::DefaultBodyLimit;
use foster_core::{Machine, MachineInstance, Snapshot};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

// ── state ─────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState<S = InMemoryStore, P = InMemoryPubSub>
where
    S: StateStore + Clone,
    P: PubSub + Clone,
{
    machines: Arc<HashMap<String, Arc<Machine>>>,
    store: S,
    pubsub: P,
    test_mode: bool,
}

// ── router ────────────────────────────────────────────────────────────────────

/// Build the Foster API router with in-memory store and pubsub (default, single-process).
///
/// For multi-replica deployments, use [`router_with`] and supply Redis-backed
/// [`StateStore`] and [`PubSub`] implementations so all replicas share state and
/// broadcast transitions across processes.
pub fn router(machines: HashMap<String, Arc<Machine>>) -> Router {
    router_with(machines, InMemoryStore::default(), InMemoryPubSub::default())
}

/// Build the Foster API router with explicit store and pubsub backends.
///
/// Enables dependency injection of custom [`StateStore`] and [`PubSub`] backends.
/// The canonical use case is a Redis-backed implementation for HA / multi-replica
/// deployments where every server replica must share state and broadcast transitions.
///
/// The `version` field on [`Snapshot`] is the optimistic lock token: a Redis Lua
/// CAS script or `WATCH`/`MULTI` transaction is the typical way to implement atomic
/// [`StateStore::apply`] in a distributed setting.
///
/// For single-process deployments, prefer [`router`] which uses in-memory defaults.
///
/// Session isolation
/// ─────────────────
/// Every request carries a `session` query param (or `"default"` if absent).
/// Instances are keyed by `(session_id, machine_id)` and created lazily on first
/// access.  The WASM client stamps `data-fx-session` on `[fx-machine]` roots so
/// Playwright can discover the session and inject state without a page reload.
///
/// Wire protocol
/// ─────────────
///   GET  /state?machine=<id>&session=<sid>  → MessagePack Snapshot
///   POST /transition                         → MessagePack TransitionRequest → MessagePack Snapshot
///   GET  /events?machine=<id>&session=<sid> → SSE stream of JSON Snapshots
///   POST /test/state?session=<sid>           → JSON Snapshot in/out  (debug only)
pub fn router_with<S, P>(
    machines: HashMap<String, Arc<Machine>>,
    store: S,
    pubsub: P,
) -> Router
where
    S: StateStore + Clone + 'static,
    P: PubSub + Clone + 'static,
{
    // Validate all templates at startup — unknown fx-show states or fx-on events panic immediately
    // rather than silently misbehaving at runtime.
    for (id, machine) in &machines {
        if let Err(errors) = machine.validate_template() {
            panic!(
                "Machine '{}' template validation failed:\n{}",
                id,
                errors.join("\n")
            );
        }
    }

    // Collect template HTML and machine metadata before machines are moved into AppState.
    let index_html = machines.values().find_map(|m| m.template.clone());

    let test_mode = cfg!(debug_assertions)
        || std::env::var("FOSTER_TEST_MODE").map(|v| v == "1").unwrap_or(false);

    // Inject overlay CSS + machine metadata in debug builds.
    // CSS is served here (not by WASM) so it's in the document before WASM runs,
    // avoiding a render cycle where the panel briefly appears as an unstyled block.
    let overlay_script = if test_mode {
        serde_json::to_string(
            &machines.iter()
                .map(|(id, m)| (id.clone(), m.state_names()))
                .collect::<HashMap<_, _>>()
        )
        .ok()
        .map(|meta| format!(
            "<style id=\"fx-dbg-css\">{OVERLAY_CSS}</style>\
             <script>window.__FOSTER_MACHINES={meta};</script>"
        ))
    } else { None };

    let app = AppState {
        machines: Arc::new(machines),
        store,
        pubsub,
        test_mode,
    };

    let mut router = Router::new()
        .route("/state", get(get_state::<S, P>))
        .route("/transition", post(post_transition::<S, P>))
        .route("/events", get(get_events::<S, P>))
        .route("/test/state", post(post_test_state::<S, P>))
        .route("/debug/history", get(get_debug_history::<S, P>))
        .route("/debug/rewind", post(post_debug_rewind::<S, P>))
        .route("/debug/graph", get(get_debug_graph::<S, P>))
        .route("/debug/timeline",   get(get_debug_timeline::<S, P>))
        .route("/debug/benchmark",  get(get_debug_benchmark::<S, P>))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .with_state(app);

    // Serve template at GET /, injecting the dev overlay in debug builds.
    if let Some(html) = index_html {
        let served = match overlay_script {
            Some(script) => html.replace("</body>", &format!("{script}</body>")),
            None => html,
        };
        router = router.route("/", get(move || async move { Html(served) }));
    }

    router
}

// ── wire helpers ──────────────────────────────────────────────────────────────

fn msgpack(data: &impl serde::Serialize) -> Response {
    match rmp_serde::to_vec_named(data) {
        Ok(bytes) => Response::builder()
            .header(header::CONTENT_TYPE, "application/msgpack")
            .body(Body::from(bytes))
            .unwrap(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn msgpack_err(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, msg.into()).into_response()
}

// ── query / request types ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct MachineQuery {
    machine: String,
    #[serde(default)]
    session: String,
}

impl MachineQuery {
    fn session_id(&self) -> &str {
        if self.session.is_empty() { "default" } else { &self.session }
    }
}

#[derive(Deserialize)]
struct SessionQuery {
    #[serde(default)]
    session: String,
}

impl SessionQuery {
    fn session_id(&self) -> &str {
        if self.session.is_empty() { "default" } else { &self.session }
    }
}

#[derive(serde::Serialize, Deserialize)]
struct TransitionRequest {
    machine: String,
    event: String,
    #[serde(default)]
    payload: Value,
    /// Session ID — set by the WASM client from the URL's `?session=` param.
    #[serde(default)]
    session: String,
}

// ── handlers ──────────────────────────────────────────────────────────────────

async fn get_state<S, P>(
    Query(q): Query<MachineQuery>,
    State(app): State<AppState<S, P>>,
) -> Response
where
    S: StateStore + Clone,
    P: PubSub + Clone,
{
    let Some(machine) = app.machines.get(&q.machine).cloned() else {
        return msgpack_err(StatusCode::NOT_FOUND, format!("machine '{}' not found", q.machine));
    };
    let snap = match app.store.load(q.session_id(), &q.machine).await {
        Some(s) => s,
        None => MachineInstance::new(machine).snapshot(),
    };
    msgpack(&snap)
}

async fn post_transition<S, P>(
    State(app): State<AppState<S, P>>,
    body: Bytes,
) -> Response
where
    S: StateStore + Clone,
    P: PubSub + Clone,
{
    let req: TransitionRequest = match rmp_serde::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return msgpack_err(StatusCode::BAD_REQUEST, e.to_string()),
    };

    let session_id = if req.session.is_empty() { "default".to_string() } else { req.session };
    let Some(machine) = app.machines.get(&req.machine).cloned() else {
        return msgpack_err(StatusCode::NOT_FOUND, format!("machine '{}' not found", req.machine));
    };

    let result = app.store.apply(&session_id, &req.machine, move |current| {
        let mut inst = match current {
            Some(snap) => {
                let mut i = MachineInstance::new(machine);
                i.restore(snap).map_err(|e| e.to_string())?;
                i
            }
            None => MachineInstance::new(machine),
        };
        inst.send(&req.event, req.payload).map_err(|e| e.to_string())
    }).await;

    match result {
        Err(e) => msgpack_err(StatusCode::BAD_REQUEST, e),
        Ok(snap) => {
            app.pubsub.publish(&session_id, &req.machine, snap.clone()).await;
            msgpack(&snap)
        }
    }
}

/// Patch event sent over SSE after the first snapshot — only the diff is sent.
#[derive(serde::Serialize)]
struct ContextPatch {
    machine_id: String,
    state: String,
    version: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_event: Option<String>,
    patch: json_patch::Patch,
}

/// Server-Sent Events stream — one channel per (session_id, machine_id).
///
/// The first event on each connection is a named `snapshot` (full state).
/// Subsequent events are named `patch` (RFC 6902 JSON Patch of just the context),
/// which reduces wire payload for large context objects (e.g. kanban task lists).
async fn get_events<S, P>(
    Query(q): Query<MachineQuery>,
    State(app): State<AppState<S, P>>,
) -> impl IntoResponse
where
    S: StateStore + Clone,
    P: PubSub + Clone,
{
    let stream = app.pubsub.subscribe(q.session_id(), &q.machine);
    let mut prev: Option<Snapshot> = None;
    let sse_stream = stream.map(move |snap| {
        let event = match prev.take() {
            None => Event::default()
                .event("snapshot")
                .json_data(&snap)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)),
            Some(p) => {
                let patch = json_patch::diff(&p.context, &snap.context);
                let msg = ContextPatch {
                    machine_id: snap.machine_id.clone(),
                    state: snap.state.clone(),
                    version: snap.version,
                    last_event: snap.last_event.clone(),
                    patch,
                };
                Event::default()
                    .event("patch")
                    .json_data(&msg)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
            }
        };
        prev = Some(snap);
        event
    });
    Sse::new(sse_stream).keep_alive(KeepAlive::default())
}

/// Debug: return the snapshot history ring buffer for a `(session, machine)` pair.
///
/// Returns oldest-first, capped at 50 entries.  Gated by `test_mode`.
async fn get_debug_history<S, P>(
    Query(q): Query<MachineQuery>,
    State(app): State<AppState<S, P>>,
) -> Response
where
    S: StateStore + Clone,
    P: PubSub + Clone,
{
    if !app.test_mode {
        return (StatusCode::FORBIDDEN, "debug endpoints are disabled in production; set FOSTER_TEST_MODE=1 to enable").into_response();
    }
    let history = app.store.history(q.session_id(), &q.machine).await;
    Json(history).into_response()
}

#[derive(Deserialize)]
struct RewindQuery {
    machine: String,
    version: u64,
    #[serde(default)]
    session: String,
}

impl RewindQuery {
    fn session_id(&self) -> &str {
        if self.session.is_empty() { "default" } else { &self.session }
    }
}

/// Rewind a machine instance to a previously recorded snapshot version.
///
/// Looks up `version` in the history ring buffer, validates state and schema via
/// `restore()`, then writes a new snapshot (at `current_version + 1`) and
/// publishes it over SSE so connected clients update immediately.
///
/// Gated by `test_mode`.
async fn post_debug_rewind<S, P>(
    Query(q): Query<RewindQuery>,
    State(app): State<AppState<S, P>>,
) -> Result<Json<Snapshot>, (StatusCode, String)>
where
    S: StateStore + Clone,
    P: PubSub + Clone,
{
    if !app.test_mode {
        return Err((StatusCode::FORBIDDEN, "debug endpoints are disabled in production; set FOSTER_TEST_MODE=1 to enable".to_string()));
    }

    let session_id = q.session_id().to_string();

    let Some(machine) = app.machines.get(&q.machine).cloned() else {
        return Err((StatusCode::NOT_FOUND, format!("machine '{}' not found", q.machine)));
    };

    let history = app.store.history(&session_id, &q.machine).await;
    let target = history
        .iter()
        .find(|s| s.version == q.version)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("version {} not in history for machine '{}'", q.version, q.machine)))?
        .clone();

    // Validate state and schema via restore() on a throwaway instance.
    let mut validator = MachineInstance::new(Arc::clone(&machine));
    validator
        .restore(Snapshot { version: 0, ..target.clone() })
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    // Build the rewound snapshot at current_version+1 so the CAS check passes.
    let current_version = app.store.load(&session_id, &q.machine).await.map(|s| s.version).unwrap_or(0);
    let rewound = Snapshot {
        machine_id: q.machine.clone(),
        state: target.state,
        context: target.context,
        version: current_version + 1,
        last_event: None,
    };

    app.store
        .store(&session_id, &q.machine, &rewound)
        .await
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
    app.pubsub.publish(&session_id, &q.machine, rewound.clone()).await;

    Ok(Json(rewound))
}

/// State graph visualiser — returns a self-contained HTML page with an SVG diagram.
///
/// The diagram shows every state (node) and every event (directed edge).  The initial
/// state is drawn with a dashed outer ring.  An SSE subscription highlights the current
/// state in green without a page reload.
///
/// Gated by `test_mode` (debug builds or `FOSTER_TEST_MODE=1`).
async fn get_debug_graph<S, P>(
    Query(q): Query<MachineQuery>,
    State(app): State<AppState<S, P>>,
) -> Response
where
    S: StateStore + Clone,
    P: PubSub + Clone,
{
    if !app.test_mode {
        return (
            StatusCode::FORBIDDEN,
            "debug endpoints are disabled in production; set FOSTER_TEST_MODE=1 to enable",
        )
            .into_response();
    }
    let Some(machine) = app.machines.get(&q.machine).cloned() else {
        return (StatusCode::NOT_FOUND, format!("machine '{}' not found", q.machine))
            .into_response();
    };
    Html(build_graph_html(&machine, q.session_id())).into_response()
}

/// Build the self-contained HTML page for a given machine's state graph.
///
/// Graph data (nodes, edges, initial state) is serialised to JSON and injected as a
/// `<script>` block that the static renderer picks up.  All rendering logic lives in
/// `GRAPH_RENDER_JS` so it never passes through `format!` and needs no brace-escaping.
fn build_graph_html(machine: &Machine, session: &str) -> String {
    let machine_id = &machine.id;
    let initial_state = &machine.initial_state;

    let nodes_json = serde_json::json!(machine.state_names()).to_string();
    let edges_json = serde_json::to_string(
        &machine
            .transitions()
            .iter()
            .map(|(f, e, t)| serde_json::json!({"from": f, "event": e, "to": t}))
            .collect::<Vec<_>>(),
    )
    .unwrap_or_default();
    let initial_json = serde_json::json!(initial_state).to_string();
    let machine_id_json = serde_json::json!(machine_id).to_string();
    let session_json = serde_json::json!(session).to_string();

    // Note: use r##"..."## so that "# sequences inside the HTML (e.g. fill="#555")
    // do not accidentally terminate the raw-string literal.
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>State Graph — {machine_id}</title>
<style>{GRAPH_CSS}</style>
</head>
<body>
<h1>State Graph — {machine_id}</h1>
<p class="meta">machine: <code>{machine_id}</code> &nbsp;|&nbsp; session: <code>{session}</code></p>
<div class="conn-dot" id="conn-dot" title="SSE connection"></div>
<svg id="graph" viewBox="0 0 800 600" xmlns="http://www.w3.org/2000/svg">
{GRAPH_SVG_DEFS}
</svg>
<script>
const NODES = {nodes_json};
const EDGES = {edges_json};
const INITIAL = {initial_json};
const MACHINE = {machine_id_json};
const SESSION = {session_json};
</script>
<script>{GRAPH_RENDER_JS}</script>
</body>
</html>"##
    )
}

/// History timeline — self-contained HTML page showing every recorded snapshot
/// as a scrubable timeline.  Clicking a step rewinds the machine to that version.
/// An auto-play button replays the sequence at configurable speed.
///
/// Gated by `test_mode`.
async fn get_debug_timeline<S: StateStore + Clone, P: PubSub + Clone>(
    Query(q): Query<MachineQuery>,
    State(app): State<AppState<S, P>>,
) -> Response {
    if !app.test_mode {
        return (
            StatusCode::FORBIDDEN,
            "debug endpoints are disabled in production; set FOSTER_TEST_MODE=1 to enable",
        )
            .into_response();
    }
    let Some(machine) = app.machines.get(&q.machine).cloned() else {
        return (StatusCode::NOT_FOUND, format!("machine '{}' not found", q.machine))
            .into_response();
    };
    Html(build_timeline_html(&machine, q.session_id())).into_response()
}

/// `GET /debug/benchmark?machine=<id>`
///
/// Walks the machine graph in-memory from the initial state, firing every
/// reachable transition in BFS order.  For each step records the full-snapshot
/// context size vs the RFC 6902 JSON Patch size relative to the previous
/// context, showing exactly how much differential SSE saves.  Returns JSON.
/// Gated by `test_mode`.
async fn get_debug_benchmark<S: StateStore + Clone, P: PubSub + Clone>(
    Query(q): Query<MachineQuery>,
    State(app): State<AppState<S, P>>,
) -> Response {
    if !app.test_mode {
        return (StatusCode::FORBIDDEN,
            "debug endpoints are disabled; set FOSTER_TEST_MODE=1").into_response();
    }
    let Some(machine) = app.machines.get(&q.machine).cloned() else {
        return (StatusCode::NOT_FOUND, format!("machine '{}' not found", q.machine))
            .into_response();
    };

    // Walk the machine graph in-memory: BFS over (state → events).
    // Each step fires the event on a fresh instance restored to that state's
    // context, so we get realistic context diffs without touching any real session.
    let inst = MachineInstance::new(Arc::clone(&machine));
    let initial_snap = inst.snapshot();
    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut prev_ctx = initial_snap.context.clone();
    let mut queue: std::collections::VecDeque<Snapshot> = std::collections::VecDeque::new();
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    queue.push_back(initial_snap);

    while let Some(snap) = queue.pop_front() {
        if !visited.insert(snap.state.clone()) { continue; }
        for (from, event, _to) in machine.transitions() {
            if from != snap.state { continue; }
            let mut step = MachineInstance::new(Arc::clone(&machine));
            if step.restore(snap.clone()).is_err() { continue; }
            let Ok(new_snap) = step.send(event, serde_json::Value::Null) else { continue };

            let full_bytes  = serde_json::to_vec(&new_snap.context).unwrap_or_default().len();
            let patch       = json_patch::diff(&prev_ctx, &new_snap.context);
            let patch_bytes = serde_json::to_vec(&patch).unwrap_or_default().len();
            let patch_pct   = if full_bytes > 0 { patch_bytes * 100 / full_bytes } else { 100 };

            rows.push(serde_json::json!({
                "from":        from,
                "event":       event,
                "to":          new_snap.state,
                "full_bytes":  full_bytes,
                "patch_bytes": patch_bytes,
                "patch_pct":   patch_pct,
            }));

            prev_ctx = new_snap.context.clone();
            queue.push_back(new_snap);
        }
    }

    let total_full:  usize = rows.iter().filter_map(|r| r["full_bytes"].as_u64()).map(|n| n as usize).sum();
    let total_patch: usize = rows.iter().filter_map(|r| r["patch_bytes"].as_u64()).map(|n| n as usize).sum();
    let overall_pct = if total_full > 0 { total_patch * 100 / total_full } else { 100 };

    axum::Json(serde_json::json!({
        "machine":            machine.id,
        "transitions_walked": rows.len(),
        "total_full_bytes":   total_full,
        "total_patch_bytes":  total_patch,
        "overall_patch_pct":  overall_pct,
        "note": "patch_pct = patch size as % of full snapshot (lower = more SSE savings)",
        "steps": rows,
    })).into_response()
}

fn build_timeline_html(machine: &Machine, session: &str) -> String {
    let machine_id      = &machine.id;
    let machine_json    = serde_json::json!(machine_id).to_string();
    let session_json    = serde_json::json!(session).to_string();
    let preview_session = format!("{session}__tl");
    let preview_json    = serde_json::json!(preview_session).to_string();
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Timeline — {machine_id}</title>
<style>{TIMELINE_CSS}</style>
</head>
<body>
<div class="tl-header">
  <span class="tl-title">Timeline — <code>{machine_id}</code></span>
  <span class="tl-meta">session: <code>{session}</code></span>
  <span class="conn-dot" id="dot"></span>
</div>
<div class="tl-toolbar">
  <button id="btn-prev"  title="Previous step">◀</button>
  <button id="btn-play"  title="Play / Pause">▶ play</button>
  <button id="btn-next"  title="Next step">▶</button>
  <button id="btn-live"  title="Jump to latest">⏭ live</button>
  <label class="speed-label">speed
    <select id="speed">
      <option value="1200">0.5×</option>
      <option value="800" selected>1×</option>
      <option value="400">2×</option>
      <option value="200">4×</option>
    </select>
  </label>
  <span class="tl-count" id="count"></span>
</div>
<div class="tl-rail-wrap">
  <div class="tl-rail" id="rail"></div>
</div>
<div class="tl-body">
  <div class="tl-preview-wrap">
    <div class="tl-panel-label">ui preview</div>
    <iframe class="tl-preview" id="preview" src="/?session={preview_session}"></iframe>
  </div>
  <div class="tl-ctx-wrap">
    <div class="tl-panel-label">context</div>
    <pre class="tl-ctx" id="ctx">—</pre>
  </div>
</div>
<script>
const MACHINE         = {machine_json};
const SESSION         = {session_json};
const PREVIEW_SESSION = {preview_json};
</script>
<script>{TIMELINE_JS}</script>
</body>
</html>"##
    )
}

const TIMELINE_CSS: &str = r#"
* { box-sizing: border-box; margin: 0; padding: 0; }
body { font-family: monospace; background: #111; color: #ccc; display: flex;
       flex-direction: column; height: 100vh; overflow: hidden; }
code { background: #222; padding: 1px 5px; border-radius: 3px; font-size: 0.85em; }
.tl-header { display: flex; align-items: center; gap: 12px; padding: 10px 16px;
             background: #1a1a1a; border-bottom: 1px solid #2a2a2a; flex-shrink: 0; }
.tl-title  { font-size: 0.95rem; color: #aaa; }
.tl-meta   { font-size: 0.8rem; color: #555; }
.conn-dot  { width: 8px; height: 8px; border-radius: 50%; background: #555; margin-left: auto; }
.conn-dot.live { background: #4caf50; }
.tl-toolbar { display: flex; align-items: center; gap: 8px; padding: 8px 16px;
              background: #161616; border-bottom: 1px solid #222; flex-shrink: 0; }
.tl-toolbar button { font-family: monospace; background: #222; border: 1px solid #333;
                     color: #aaa; padding: 3px 10px; cursor: pointer; border-radius: 3px;
                     font-size: 0.82rem; }
.tl-toolbar button:hover { background: #2a2a2a; color: #ddd; }
.tl-toolbar button.playing { background: #1a3a1a; border-color: #2d6a2d; color: #4caf50; }
#btn-live { margin-left: 4px; }
.speed-label { font-size: 0.78rem; color: #555; margin-left: 8px; }
.speed-label select { font-family: monospace; background: #1a1a1a; border: 1px solid #2a2a2a;
                      color: #888; border-radius: 3px; padding: 2px 4px; font-size: 0.78rem; }
.tl-count  { font-size: 0.78rem; color: #555; margin-left: auto; }
.tl-rail-wrap { height: 130px; flex-shrink: 0; overflow-x: auto; overflow-y: hidden;
                padding: 16px 16px; border-bottom: 1px solid #1e1e1e; }
.tl-rail { display: flex; align-items: center; min-width: max-content; gap: 0; height: 100%; }
.tl-snap { display: flex; flex-direction: column; align-items: center; gap: 6px;
           padding: 10px 14px; border: 1px solid #2a2a2a; border-radius: 6px;
           background: #1a1a1a; cursor: pointer; min-width: 100px; max-width: 140px;
           transition: border-color 0.12s, background 0.12s; }
.tl-snap:hover { border-color: #444; background: #1e1e1e; }
.tl-snap.active { border-color: #4caf50; background: #0d2010; }
.tl-snap-ver   { font-size: 0.68rem; color: #555; }
.tl-snap-state { font-size: 0.85rem; color: #ddd; font-weight: 600; text-align: center;
                 word-break: break-all; }
.tl-snap.active .tl-snap-state { color: #4caf50; }
.tl-snap-ev    { font-size: 0.7rem; color: #666; text-align: center; word-break: break-all; }
.tl-connector  { width: 32px; flex-shrink: 0; height: 2px; background: #2a2a2a;
                 position: relative; }
.tl-connector::after { content: '▶'; position: absolute; right: -6px; top: -7px;
                       font-size: 10px; color: #333; }
/* ── split body: iframe preview + context JSON ─────────────────────────────── */
.tl-body { display: flex; flex: 1 1 auto; min-height: 0; overflow: hidden; }
.tl-preview-wrap { flex: 1 1 auto; display: flex; flex-direction: column; min-width: 0;
                   border-right: 1px solid #2a2a2a; }
.tl-preview { flex: 1; border: none; width: 100%; background: #fff; }
.tl-ctx-wrap { flex: 0 0 320px; display: flex; flex-direction: column; overflow: hidden; }
.tl-ctx { flex: 1; font-family: monospace; font-size: 0.8rem; color: #888;
          white-space: pre-wrap; word-break: break-all; line-height: 1.5;
          overflow: auto; padding: 12px 16px; }
.tl-panel-label { font-size: 0.7rem; color: #555; text-transform: uppercase;
                  letter-spacing: 0.1em; padding: 6px 16px 5px;
                  border-bottom: 1px solid #1e1e1e; flex-shrink: 0; }
"#;

const TIMELINE_JS: &str = r#"
(function () {
  const HISTORY_URL = `/debug/history?machine=${encodeURIComponent(MACHINE)}&session=${encodeURIComponent(SESSION)}`;
  const REWIND_URL  = v => `/debug/rewind?machine=${encodeURIComponent(MACHINE)}&session=${encodeURIComponent(SESSION)}&version=${v}`;
  const SSE_URL     = `/events?machine=${encodeURIComponent(MACHINE)}&session=${encodeURIComponent(SESSION)}`;

  let history  = [];
  let activeVer = null;
  let playTimer = null;
  let ignoreVer = null; // version we just rewound to — suppress echo

  // ── data fetch ──────────────────────────────────────────────────────────────
  function loadHistory() {
    return fetch(HISTORY_URL).then(r => r.json()).then(snaps => {
      history = snaps;
      render();
      updateCount();
    });
  }

  // ── render timeline ──────────────────────────────────────────────────────────
  function render() {
    const rail = document.getElementById('rail');
    rail.innerHTML = '';
    history.forEach((snap, i) => {
      if (i > 0) {
        const line = document.createElement('div');
        line.className = 'tl-connector';
        rail.appendChild(line);
      }
      const el = document.createElement('div');
      el.className = 'tl-snap' + (snap.version === activeVer ? ' active' : '');
      el.dataset.version = snap.version;
      el.innerHTML =
        `<div class="tl-snap-ver">v${snap.version}</div>` +
        `<div class="tl-snap-state">${snap.state}</div>` +
        `<div class="tl-snap-ev">${snap.last_event || '—'}</div>`;
      el.addEventListener('click', () => rewindTo(snap.version));
      rail.appendChild(el);
    });
    if (activeVer !== null) scrollToActive();
  }

  function updateCount() {
    const idx = history.findIndex(s => s.version === activeVer);
    const label = idx >= 0
      ? `step ${idx + 1} / ${history.length}`
      : `${history.length} steps`;
    document.getElementById('count').textContent = label;
  }

  // ── preview injection ─────────────────────────────────────────────────────────
  // Injects a historical snapshot into the isolated preview session so the iframe
  // renders the UI exactly as it was at that point in time.
  function injectPreview(snap) {
    if (!snap) return;
    fetch(`/test/state?session=${encodeURIComponent(PREVIEW_SESSION)}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        machine_id: MACHINE,
        state:      snap.state,
        context:    snap.context,
        version:    snap.version,
      }),
    });
  }

  // ── activation ───────────────────────────────────────────────────────────────
  function activate(version) {
    activeVer = version;
    document.querySelectorAll('.tl-snap').forEach(el => {
      el.classList.toggle('active', +el.dataset.version === version);
    });
    const snap = history.find(s => s.version === version);
    document.getElementById('ctx').textContent = snap
      ? JSON.stringify(snap.context, null, 2)
      : '—';
    updateCount();
    scrollToActive();
    injectPreview(snap);
  }

  function scrollToActive() {
    const el = document.querySelector(`.tl-snap[data-version="${activeVer}"]`);
    if (el) el.scrollIntoView({ behavior: 'smooth', block: 'nearest', inline: 'center' });
  }

  // ── rewind ────────────────────────────────────────────────────────────────────
  function rewindTo(version) {
    ignoreVer = version;
    fetch(REWIND_URL(version), { method: 'POST' });
    activate(version);
  }

  // ── play controls ─────────────────────────────────────────────────────────────
  const btnPlay = document.getElementById('btn-play');

  function stopPlay() {
    if (!playTimer) return;
    clearInterval(playTimer);
    playTimer = null;
    btnPlay.textContent = '▶ play';
    btnPlay.classList.remove('playing');
  }

  function startPlay() {
    stopPlay();
    let idx = history.findIndex(s => s.version === activeVer);
    if (idx < 0 || idx >= history.length - 1) idx = -1; // restart from top
    const speed = +document.getElementById('speed').value;
    btnPlay.textContent = '⏸ pause';
    btnPlay.classList.add('playing');
    playTimer = setInterval(() => {
      idx++;
      if (idx >= history.length) { stopPlay(); return; }
      rewindTo(history[idx].version);
    }, speed);
  }

  btnPlay.addEventListener('click', () => playTimer ? stopPlay() : startPlay());

  document.getElementById('btn-prev').addEventListener('click', () => {
    stopPlay();
    const idx = history.findIndex(s => s.version === activeVer);
    if (idx > 0) rewindTo(history[idx - 1].version);
  });

  document.getElementById('btn-next').addEventListener('click', () => {
    stopPlay();
    const idx = history.findIndex(s => s.version === activeVer);
    if (idx >= 0 && idx < history.length - 1) rewindTo(history[idx + 1].version);
  });

  document.getElementById('btn-live').addEventListener('click', () => {
    stopPlay();
    loadHistory().then(() => {
      if (history.length) rewindTo(history[history.length - 1].version);
    });
  });

  // ── SSE — live tail ───────────────────────────────────────────────────────────
  const dot = document.getElementById('dot');
  const es  = new EventSource(SSE_URL);
  es.onopen  = () => dot.classList.add('live');
  es.onerror = () => dot.classList.remove('live');

  const onSseSnap = ev => {
    let snap;
    try { snap = JSON.parse(ev.data); } catch (_) { return; }
    // Rewind echo: server bumps version, so ignore until next live event.
    if (snap.version === ignoreVer) { ignoreVer = null; return; }
    ignoreVer = null;
    if (!history.find(s => s.version === snap.version)) {
      history.push(snap);
      render();
    }
    activate(snap.version);
  };

  // patch events don't carry full context — refetch history then activate latest
  const onSsePatch = () => {
    if (ignoreVer !== null) { ignoreVer = null; return; }
    loadHistory().then(() => {
      if (history.length) activate(history[history.length - 1].version);
    });
  };

  es.addEventListener('snapshot', onSseSnap);
  es.addEventListener('patch',    onSsePatch);

  // ── init ──────────────────────────────────────────────────────────────────────
  loadHistory().then(() => {
    if (history.length) activate(history[history.length - 1].version);
  });
}());
"#;

// ── overlay CSS (served by the server so it's in the HTML before WASM runs) ───
const OVERLAY_CSS: &str = "\
.fx-dbg{position:fixed;bottom:14px;right:14px;z-index:2147483647;font-family:monospace;font-size:12px;background:#1a1a1a;border:1px solid #333;border-radius:8px;min-width:210px;box-shadow:0 4px 24px rgba(0,0,0,.6);color:#ccc}\
.fx-dbg.min .fx-dbg-body{display:none}\
.fx-dbg-head{display:flex;align-items:center;padding:7px 10px;gap:6px;background:#242424;border-radius:7px 7px 0 0}\
.fx-dbg-body{display:flex;flex-direction:column;padding:8px 10px;gap:5px}\
.fx-dbg-row{display:flex;justify-content:space-between;align-items:center}\
.fx-dbg-key{color:#666}\
.fx-dbg-st{display:inline-block;padding:1px 8px;border-radius:10px;background:#1a3a1a;color:#4caf50}\
.fx-dbg-jump{display:flex;gap:4px;margin-top:2px}\
.fx-dbg-jump select{flex:1;background:#111;color:#ccc;border:1px solid #333;border-radius:4px;padding:2px 4px;font:11px monospace}\
.fx-dbg-jump button{background:#2a4a2a;color:#4caf50;border:1px solid #2d6a2d;border-radius:4px;padding:2px 8px;cursor:pointer;font-size:11px}\
.fx-dbg-links{display:flex;gap:8px;margin-top:4px}\
.fx-dbg-links a{color:#4a9eff;text-decoration:none;font-size:11px}\
.fx-dbg-ctrl{background:none;border:none;color:#666;cursor:pointer;padding:0 2px;font-size:13px}\
";

// r##"..."## is required: fill="#555" contains "# which would terminate r#"..."#.
const GRAPH_SVG_DEFS: &str = r##"<defs>
  <marker id="arrow" markerWidth="8" markerHeight="8" refX="7" refY="3" orient="auto" markerUnits="strokeWidth">
    <path d="M0,0 L0,6 L8,3 z" fill="#555"/>
  </marker>
</defs>"##;

const GRAPH_CSS: &str = r#"
body { font-family: system-ui, sans-serif; background: #111; color: #eee; margin: 0; padding: 16px; }
h1   { margin: 0 0 6px; font-size: 1.1rem; color: #aaa; font-weight: normal; }
.meta { font-size: 0.8rem; color: #666; margin-bottom: 14px; }
code { background: #222; padding: 2px 6px; border-radius: 3px; }
svg  { display: block; width: 100%; max-width: 900px; background: #1a1a1a;
       border-radius: 8px; border: 1px solid #333; }
.edge-path  { fill: none; stroke: #555; stroke-width: 1.5; }
.edge-label { font-size: 10px; fill: #888; text-anchor: middle; dominant-baseline: middle; }
.edge-bg    { fill: #1a1a1a; }
.state-label { font-size: 13px; fill: #eee; text-anchor: middle; dominant-baseline: middle;
               pointer-events: none; }
.node-ring  { fill: #242424; stroke-width: 2; }
.n-normal   { stroke: #444; }
.n-initial  { stroke: #4a9eff; }
.n-current  { stroke: #4caf50; fill: #1a3a1a; }
.conn-dot   { position: fixed; top: 16px; right: 16px; width: 8px; height: 8px;
              border-radius: 50%; background: #555; }
.conn-dot.live { background: #4caf50; }
"#;

/// Pure JS — no Rust format args, so braces need no escaping.
const GRAPH_RENDER_JS: &str = r#"
(function () {
  const svg = document.getElementById('graph');
  const W = 800, H = 600, CX = W / 2, CY = H / 2, NR = 34;

  function mkEl(tag, attrs) {
    const el = document.createElementNS('http://www.w3.org/2000/svg', tag);
    for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
    return el;
  }

  // ── layout ──────────────────────────────────────────────────────────────────
  const pos = {};
  if (NODES.length === 1) {
    pos[NODES[0]] = { x: CX, y: CY };
  } else {
    const R = Math.min(W, H) * 0.33;
    NODES.forEach((name, i) => {
      const angle = (2 * Math.PI * i / NODES.length) - Math.PI / 2;
      pos[name] = { x: CX + R * Math.cos(angle), y: CY + R * Math.sin(angle) };
    });
  }

  // ── group edges by (from, to) ────────────────────────────────────────────
  const SEP = '\x00';
  const groups = {};
  EDGES.forEach(e => {
    const key = e.from + SEP + e.to;
    if (!groups[key]) groups[key] = { from: e.from, to: e.to, events: [] };
    groups[key].events.push(e.event);
  });
  function hasBidi(a, b) { return !!(groups[b + SEP + a]); }

  // ── draw edges ───────────────────────────────────────────────────────────
  // Inserted before nodes so nodes appear on top.
  const edgeLayer = mkEl('g', {});
  svg.appendChild(edgeLayer);

  Object.values(groups).forEach(({ from, to, events }) => {
    const label = events.join(' / ');
    const fp = pos[from], tp = pos[to];
    let pathD, lx, ly;

    if (from === to) {
      // Self-loop: small arc above the node
      const sx = fp.x - NR * 0.7, sy = fp.y - NR;
      const ex = fp.x + NR * 0.7, ey = fp.y - NR;
      pathD = `M ${sx} ${sy} C ${sx} ${sy - 44} ${ex} ${ey - 44} ${ex} ${ey}`;
      lx = fp.x; ly = fp.y - NR - 32;
    } else {
      const dx = tp.x - fp.x, dy = tp.y - fp.y;
      const len = Math.sqrt(dx * dx + dy * dy) || 1;
      const ux = dx / len, uy = dy / len;
      // Shorten to node boundary
      const sx = fp.x + ux * NR, sy = fp.y + uy * NR;
      const ex = tp.x - ux * (NR + 6), ey = tp.y - uy * (NR + 6);

      if (hasBidi(from, to)) {
        // Curve to avoid overlap with the reverse edge
        const ox = -uy * 44, oy = ux * 44;
        const mx = (sx + ex) / 2 + ox, my = (sy + ey) / 2 + oy;
        pathD = `M ${sx} ${sy} Q ${mx} ${my} ${ex} ${ey}`;
        lx = mx; ly = my;
      } else {
        pathD = `M ${sx} ${sy} L ${ex} ${ey}`;
        lx = (sx + ex) / 2; ly = (sy + ey) / 2;
      }
    }

    const path = mkEl('path', { d: pathD, class: 'edge-path', 'marker-end': 'url(#arrow)' });
    edgeLayer.appendChild(path);

    // Label — small text with a background rect for legibility
    const charW = 5.5, pad = 4;
    const lw = label.length * charW + pad * 2, lh = 14;
    const bg = mkEl('rect', {
      x: lx - lw / 2, y: ly - lh / 2, width: lw, height: lh, rx: 2, class: 'edge-bg'
    });
    const txt = mkEl('text', { x: lx, y: ly, class: 'edge-label' });
    txt.textContent = label;
    edgeLayer.appendChild(bg);
    edgeLayer.appendChild(txt);
  });

  // ── draw nodes ───────────────────────────────────────────────────────────
  const circles = {};
  NODES.forEach(name => {
    const { x, y } = pos[name];
    const isInitial = name === INITIAL;
    const g = mkEl('g', {});

    if (isInitial) {
      // Dashed outer ring marks the initial state
      g.appendChild(mkEl('circle', {
        cx: x, cy: y, r: NR + 6,
        fill: 'none', stroke: '#4a9eff', 'stroke-width': 1, 'stroke-dasharray': '3,3'
      }));
    }

    const circle = mkEl('circle', { cx: x, cy: y, r: NR, class: 'node-ring n-' + (isInitial ? 'initial' : 'normal') });
    const text   = mkEl('text',   { x, y, class: 'state-label' });
    text.textContent = name;
    g.appendChild(circle);
    g.appendChild(text);
    svg.appendChild(g);
    circles[name] = circle;
  });

  // ── SSE — highlight current state ────────────────────────────────────────
  let currentState = null;
  function setCurrentState(state) {
    if (currentState === state) return;
    if (currentState && circles[currentState]) {
      const prev = currentState === INITIAL ? 'n-initial' : 'n-normal';
      circles[currentState].setAttribute('class', 'node-ring ' + prev);
    }
    currentState = state;
    if (state && circles[state]) {
      circles[state].setAttribute('class', 'node-ring n-current');
    }
  }

  const dot = document.getElementById('conn-dot');
  const url = `/events?machine=${encodeURIComponent(MACHINE)}&session=${encodeURIComponent(SESSION)}`;
  const es = new EventSource(url);
  es.onopen  = () => dot.classList.add('live');
  es.onerror = () => dot.classList.remove('live');
  const onState = ev => { try { setCurrentState(JSON.parse(ev.data).state); } catch (_) {} };
  es.addEventListener('snapshot', onState);
  es.addEventListener('patch',    onState);
}());
"#;

/// Inject an arbitrary snapshot — bypasses all transition logic.
///
/// Gated by `test_mode` (on in debug builds, off in release unless
/// `FOSTER_TEST_MODE=1`).  Returns 403 in production.
///
/// curl example:
///   curl -X POST 'http://localhost:3000/test/state?session=my-test' \
///        -H 'Content-Type: application/json' \
///        -d '{"machine_id":"counter","state":"error","context":{"count":99},"version":0}'
async fn post_test_state<S, P>(
    Query(q): Query<SessionQuery>,
    State(app): State<AppState<S, P>>,
    Json(snap): Json<Snapshot>,
) -> Result<Json<Snapshot>, (StatusCode, String)>
where
    S: StateStore + Clone,
    P: PubSub + Clone,
{
    if !app.test_mode {
        return Err((StatusCode::FORBIDDEN, "test endpoints are disabled in production; set FOSTER_TEST_MODE=1 to enable".to_string()));
    }

    let session_id = q.session_id().to_string();
    let machine_id = snap.machine_id.clone();

    let Some(machine) = app.machines.get(&machine_id).cloned() else {
        return Err((StatusCode::NOT_FOUND, format!("machine '{machine_id}' not found")));
    };

    let mut inst = MachineInstance::new(machine);
    inst.restore(snap.clone()).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let restored = inst.snapshot();

    app.store.store(&session_id, &machine_id, &restored).await
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
    app.pubsub.publish(&session_id, &machine_id, restored.clone()).await;

    Ok(Json(restored))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use foster_core::MachineBuilder;
    use serde_json::json;

    fn two_state_machine() -> Arc<Machine> {
        MachineBuilder::new("counter", "idle", json!({ "count": 0 }))
            .state("error")
            .on("idle", "increment", "idle", |ctx, _| {
                Ok(json!({ "count": ctx["count"].as_i64().unwrap_or(0) + 1 }))
            })
            .pass("idle", "break_it", "error")
            .pass("error", "recover", "idle")
            .build()
    }

    #[test]
    fn graph_html_contains_machine_id() {
        let m = two_state_machine();
        let html = build_graph_html(&m, "default");
        assert!(html.contains("counter"), "machine id missing from graph html");
    }

    #[test]
    fn graph_html_contains_all_states_in_json() {
        let m = two_state_machine();
        let html = build_graph_html(&m, "default");
        assert!(html.contains("\"idle\""), "state 'idle' missing from graph html");
        assert!(html.contains("\"error\""), "state 'error' missing from graph html");
    }

    #[test]
    fn graph_html_contains_all_events_in_json() {
        let m = two_state_machine();
        let html = build_graph_html(&m, "default");
        assert!(html.contains("\"increment\""), "event 'increment' missing");
        assert!(html.contains("\"break_it\""), "event 'break_it' missing");
        assert!(html.contains("\"recover\""), "event 'recover' missing");
    }

    #[test]
    fn graph_html_marks_initial_state() {
        let m = two_state_machine();
        let html = build_graph_html(&m, "default");
        // INITIAL constant must appear as the JSON string "idle"
        assert!(html.contains(r#"const INITIAL = "idle""#), "INITIAL constant missing or wrong");
    }

    #[test]
    fn graph_html_embeds_session_and_machine_constants() {
        let m = two_state_machine();
        let html = build_graph_html(&m, "my-session");
        assert!(html.contains(r#"const SESSION = "my-session""#));
        assert!(html.contains(r#"const MACHINE = "counter""#));
    }

    #[test]
    fn graph_html_is_valid_document_shell() {
        let m = two_state_machine();
        let html = build_graph_html(&m, "default");
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<svg"));
        assert!(html.contains("</html>"));
    }

    #[test]
    fn graph_html_single_state_machine() {
        let m = MachineBuilder::new("alone", "only", json!({}))
            .pass("only", "noop", "only")
            .build();
        let html = build_graph_html(&m, "s");
        assert!(html.contains("\"only\""));
        assert!(html.contains("\"noop\""));
    }

    // ── HTTP integration tests ────────────────────────────────────────────────

    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_machines() -> HashMap<String, Arc<Machine>> {
        HashMap::from([("counter".to_string(), two_state_machine())])
    }

    #[tokio::test]
    async fn http_get_state_returns_initial_snapshot() {
        let app = router(test_machines());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/state?machine=counter")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let snap: Snapshot = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(snap.machine_id, "counter");
        assert_eq!(snap.state, "idle");
        assert_eq!(snap.version, 0);
        assert_eq!(snap.context["count"], 0);
    }

    #[tokio::test]
    async fn http_get_state_404_for_unknown_machine() {
        let app = router(test_machines());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/state?machine=unknown")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn http_post_transition_increments_counter() {
        let app = router(test_machines());
        let req_body = TransitionRequest {
            machine: "counter".to_string(),
            event: "increment".to_string(),
            payload: Value::Null,
            session: "t1".to_string(),
        };
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transition")
                    .body(Body::from(rmp_serde::to_vec_named(&req_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let snap: Snapshot = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(snap.state, "idle");
        assert_eq!(snap.context["count"], 1);
        assert_eq!(snap.version, 1);
        assert_eq!(snap.last_event.as_deref(), Some("increment"));
    }

    #[tokio::test]
    async fn http_post_transition_state_change() {
        let app = router(test_machines());
        let req_body = TransitionRequest {
            machine: "counter".to_string(),
            event: "break_it".to_string(),
            payload: Value::Null,
            session: "t2".to_string(),
        };
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transition")
                    .body(Body::from(rmp_serde::to_vec_named(&req_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let snap: Snapshot = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(snap.state, "error");
    }

    #[tokio::test]
    async fn http_post_transition_404_for_unknown_machine() {
        let app = router(test_machines());
        let req_body = TransitionRequest {
            machine: "nope".to_string(),
            event: "x".to_string(),
            payload: Value::Null,
            session: "t3".to_string(),
        };
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transition")
                    .body(Body::from(rmp_serde::to_vec_named(&req_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn http_post_transition_400_for_invalid_event() {
        let app = router(test_machines());
        let req_body = TransitionRequest {
            machine: "counter".to_string(),
            event: "no_such_event".to_string(),
            payload: Value::Null,
            session: "t4".to_string(),
        };
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transition")
                    .body(Body::from(rmp_serde::to_vec_named(&req_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn http_post_test_state_injects_snapshot() {
        let app = router(test_machines());
        let snap = Snapshot {
            machine_id: "counter".to_string(),
            state: "error".to_string(),
            context: json!({ "count": 99 }),
            version: 0,
            last_event: None,
        };
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/test/state?session=inject-test")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&snap).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let returned: Snapshot = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(returned.state, "error");
        assert_eq!(returned.context["count"], 99);
    }

    #[tokio::test]
    async fn http_debug_history_records_transitions() {
        let store = InMemoryStore::default();
        let pubsub = InMemoryPubSub::default();
        let app = router_with(test_machines(), store, pubsub);

        // Fire a transition
        let req_body = TransitionRequest {
            machine: "counter".to_string(),
            event: "increment".to_string(),
            payload: Value::Null,
            session: "hist-s".to_string(),
        };
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transition")
                    .body(Body::from(rmp_serde::to_vec_named(&req_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Check history
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/debug/history?machine=counter&session=hist-s")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let history: Vec<Snapshot> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].state, "idle");
        assert_eq!(history[0].context["count"], 1);
    }

    #[tokio::test]
    async fn router_with_accepts_custom_backends() {
        let app = router_with(
            test_machines(),
            InMemoryStore::default(),
            InMemoryPubSub::default(),
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/state?machine=counter")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let snap: Snapshot = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(snap.machine_id, "counter");
        assert_eq!(snap.state, "idle");
    }

    #[tokio::test]
    async fn ha_shared_store_state_visible_across_replicas() {
        let store = InMemoryStore::default();
        let snap_a = Snapshot {
            machine_id: "counter".into(),
            state: "idle".into(),
            context: json!({ "count": 42 }),
            version: 1,
            last_event: Some("increment".into()),
        };
        store.store("session1", "counter", &snap_a).await.unwrap();
        let loaded = store.clone().load("session1", "counter").await.unwrap();
        assert_eq!(loaded.state, "idle");
        assert_eq!(loaded.context["count"], 42);
        assert_eq!(loaded.version, 1);
    }

    #[tokio::test]
    async fn ha_shared_store_history_visible_across_replicas() {
        let store = InMemoryStore::default();
        let store_b = store.clone();
        store.store("s", "m", &Snapshot { machine_id: "m".into(), state: "a".into(), context: json!({}), version: 1, last_event: None }).await.unwrap();
        store.store("s", "m", &Snapshot { machine_id: "m".into(), state: "b".into(), context: json!({}), version: 2, last_event: None }).await.unwrap();
        let history = store_b.history("s", "m").await;
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].state, "a");
        assert_eq!(history[1].state, "b");
    }
}
