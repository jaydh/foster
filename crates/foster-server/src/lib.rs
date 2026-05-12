use axum::{
    body::{Body, Bytes},
    extract::{Query, State},
    http::{header, StatusCode},
    response::{sse::{Event, KeepAlive, Sse}, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use axum::extract::DefaultBodyLimit;
use foster_core::{Machine, MachineInstance, Snapshot};
use futures_util::stream;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

// ── state ─────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    /// Static machine definitions — keyed by machine_id.
    machines: Arc<HashMap<String, Arc<Machine>>>,
    /// Live instances — keyed by (session_id, machine_id), created on first access.
    instances: Arc<Mutex<HashMap<(String, String), MachineInstance>>>,
    /// SSE broadcast channels — keyed by (session_id, machine_id).
    broadcasts: Arc<Mutex<HashMap<(String, String), broadcast::Sender<Snapshot>>>>,
    /// When false, `POST /test/state` returns 403.  Defaults to true in debug builds.
    test_mode: bool,
}

impl AppState {
    fn session_instance<T>(
        &self,
        session_id: &str,
        machine_id: &str,
        f: impl FnOnce(&mut MachineInstance) -> T,
    ) -> Option<T> {
        let mut instances = self.instances.lock().unwrap();
        let key = (session_id.to_string(), machine_id.to_string());
        if !instances.contains_key(&key) {
            let machine = self.machines.get(machine_id)?.clone();
            instances.insert(key.clone(), MachineInstance::new(machine));
        }
        instances.get_mut(&key).map(f)
    }

    fn broadcast(&self, session_id: &str, machine_id: &str, snap: &Snapshot) {
        let tx = self
            .broadcasts
            .lock()
            .unwrap()
            .get(&(session_id.to_string(), machine_id.to_string()))
            .cloned();
        if let Some(tx) = tx {
            let _ = tx.send(snap.clone());
        }
    }
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
    let test_mode = cfg!(debug_assertions)
        || std::env::var("FOSTER_TEST_MODE").map(|v| v == "1").unwrap_or(false);

    let app = AppState {
        machines: Arc::new(machines),
        instances: Arc::new(Mutex::new(HashMap::new())),
        broadcasts: Arc::new(Mutex::new(HashMap::new())),
        test_mode,
    };

    Router::new()
        .route("/state", get(get_state))
        .route("/transition", post(post_transition))
        .route("/events", get(get_events))
        .route("/test/state", post(post_test_state))
        .layer(DefaultBodyLimit::max(256 * 1024)) // 256 KB max body
        .with_state(app)
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
    match app.session_instance(q.session_id(), &q.machine, |inst| inst.snapshot()) {
        Some(snap) => msgpack(&snap),
        None => msgpack_err(StatusCode::NOT_FOUND, format!("machine '{}' not found", q.machine)),
    }
}

async fn post_transition(
    State(app): State<AppState>,
    body: Bytes,
) -> Response {
    let req: TransitionRequest = match rmp_serde::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return msgpack_err(StatusCode::BAD_REQUEST, e.to_string()),
    };

    let session_id = if req.session.is_empty() { "default".to_string() } else { req.session.clone() };

    let result = app.session_instance(&session_id, &req.machine, |inst| {
        inst.send(&req.event, req.payload.clone())
    });

    match result {
        None => msgpack_err(StatusCode::NOT_FOUND, format!("machine '{}' not found", req.machine)),
        Some(Err(e)) => msgpack_err(StatusCode::BAD_REQUEST, e.to_string()),
        Some(Ok(snap)) => {
            app.broadcast(&session_id, &req.machine, &snap);
            msgpack(&snap)
        }
    }
}

/// Server-Sent Events stream — one channel per (session_id, machine_id).
/// After any state change (`POST /transition` or `POST /test/state`), the new
/// snapshot is pushed here so the WASM client can update without a page reload.
async fn get_events(
    Query(q): Query<MachineQuery>,
    State(app): State<AppState>,
) -> impl IntoResponse {
    let key = (q.session_id().to_string(), q.machine.clone());

    let rx = {
        let mut broadcasts = app.broadcasts.lock().unwrap();
        broadcasts
            .entry(key)
            .or_insert_with(|| broadcast::channel(64).0)
            .subscribe()
    };

    // Drive the broadcast Receiver as an SSE stream.
    // stream::unfold avoids pulling in a StreamExt import that may conflict.
    let sse_stream = stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(snap) => {
                    if let Ok(event) = Event::default().json_data(snap) {
                        return Some((Ok::<_, std::convert::Infallible>(event), rx));
                    }
                    // json_data failed (shouldn't happen) — skip this item
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed)    => return None,
            }
        }
    });

    Sse::new(sse_stream).keep_alive(KeepAlive::default())
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

    let result = app.session_instance(&session_id, &machine_id, |inst| {
        inst.restore(snap.clone()).map(|()| inst.snapshot())
    });

    let restored = match result {
        None => return Err((StatusCode::NOT_FOUND, format!("machine '{machine_id}' not found"))),
        Some(Err(e)) => return Err((StatusCode::BAD_REQUEST, e.to_string())),
        Some(Ok(s)) => s,
    };

    app.broadcast(&session_id, &machine_id, &restored);
    Ok(Json(restored))
}
