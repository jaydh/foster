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

    let current = app.store.load(&session_id, &req.machine).await;
    let mut inst = match current {
        Some(snap) => {
            let mut i = MachineInstance::new(machine);
            if let Err(e) = i.restore(snap) {
                return msgpack_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
            }
            i
        }
        None => MachineInstance::new(machine),
    };

    let snap = match inst.send(&req.event, req.payload) {
        Err(e) => return msgpack_err(StatusCode::BAD_REQUEST, e.to_string()),
        Ok(s) => s,
    };

    if let Err(e) = app.store.store(&session_id, &req.machine, &snap).await {
        return msgpack_err(StatusCode::CONFLICT, e.to_string());
    }

    app.pubsub.publish(&session_id, &req.machine, snap.clone()).await;
    msgpack(&snap)
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
