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
struct AppState {
    machines: Arc<HashMap<String, Arc<Machine>>>,
    store: InMemoryStore,
    pubsub: InMemoryPubSub,
    test_mode: bool,
}

// ── router ────────────────────────────────────────────────────────────────────

/// Build the Foster API router pre-loaded with machine definitions.
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
pub fn router(machines: HashMap<String, Arc<Machine>>) -> Router {
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

    // Collect template HTML before machines are moved into AppState.
    let index_html = machines.values().find_map(|m| m.template.clone());

    let test_mode = cfg!(debug_assertions)
        || std::env::var("FOSTER_TEST_MODE").map(|v| v == "1").unwrap_or(false);

    let app = AppState {
        machines: Arc::new(machines),
        store: InMemoryStore::default(),
        pubsub: InMemoryPubSub::default(),
        test_mode,
    };

    let mut router = Router::new()
        .route("/state", get(get_state))
        .route("/transition", post(post_transition))
        .route("/events", get(get_events))
        .route("/test/state", post(post_test_state))
        .route("/debug/history", get(get_debug_history))
        .route("/debug/rewind", post(post_debug_rewind))
        .route("/debug/graph", get(get_debug_graph))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .with_state(app);

    // If a machine provides a template, register GET / so examples need no explicit index handler.
    // Explicit routes take priority over any ServeDir the caller nests afterward.
    if let Some(html) = index_html {
        router = router.route("/", get(move || async move { Html(html) }));
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

#[derive(Deserialize)]
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

async fn get_state(
    Query(q): Query<MachineQuery>,
    State(app): State<AppState>,
) -> Response {
    let Some(machine) = app.machines.get(&q.machine).cloned() else {
        return msgpack_err(StatusCode::NOT_FOUND, format!("machine '{}' not found", q.machine));
    };
    let snap = match app.store.load(q.session_id(), &q.machine).await {
        Some(s) => s,
        None => MachineInstance::new(machine).snapshot(),
    };
    msgpack(&snap)
}

async fn post_transition(
    State(app): State<AppState>,
    body: Bytes,
) -> Response {
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

/// Server-Sent Events stream — one channel per (session_id, machine_id).
/// After any state change, the new snapshot is pushed here so the WASM client
/// can update without a page reload.
async fn get_events(
    Query(q): Query<MachineQuery>,
    State(app): State<AppState>,
) -> impl IntoResponse {
    let stream = app.pubsub.subscribe(q.session_id(), &q.machine);
    let sse_stream = stream.map(|snap| {
        Event::default().json_data(snap)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    });
    Sse::new(sse_stream).keep_alive(KeepAlive::default())
}

/// Debug: return the snapshot history ring buffer for a `(session, machine)` pair.
///
/// Returns oldest-first, capped at 50 entries.  Gated by `test_mode`.
async fn get_debug_history(
    Query(q): Query<MachineQuery>,
    State(app): State<AppState>,
) -> Response {
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
async fn post_debug_rewind(
    Query(q): Query<RewindQuery>,
    State(app): State<AppState>,
) -> Result<Json<Snapshot>, (StatusCode, String)> {
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
async fn get_debug_graph(
    Query(q): Query<MachineQuery>,
    State(app): State<AppState>,
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
  es.onopen    = () => dot.classList.add('live');
  es.onerror   = () => dot.classList.remove('live');
  es.onmessage = ev => { try { setCurrentState(JSON.parse(ev.data).state); } catch (_) {} };
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
async fn post_test_state(
    Query(q): Query<SessionQuery>,
    State(app): State<AppState>,
    Json(snap): Json<Snapshot>,
) -> Result<Json<Snapshot>, (StatusCode, String)> {
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
}
