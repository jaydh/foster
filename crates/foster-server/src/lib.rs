use axum::{
    body::Body,
    body::Bytes,
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use foster_core::{Machine, MachineInstance, Snapshot};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

type Sessions = Arc<Mutex<HashMap<String, MachineInstance>>>;

/// Build the Foster API router pre-loaded with machine definitions.
///
/// Wire protocol
/// ─────────────
///   GET  /state?machine=<id>   → MessagePack Snapshot
///   POST /transition            → MessagePack TransitionRequest → MessagePack Snapshot
///   POST /test/state            → JSON Snapshot → JSON Snapshot  (Playwright / curl)
///
/// `/test/state` intentionally stays JSON so it is trivially curl-able during debugging
/// without needing a msgpack encoder on the command line.
pub fn router(machines: HashMap<String, Arc<Machine>>) -> Router {
    let sessions: Sessions = Arc::new(Mutex::new(
        machines
            .into_iter()
            .map(|(id, m)| (id, MachineInstance::new(m)))
            .collect(),
    ));

    Router::new()
        .route("/state", get(get_state))
        .route("/transition", post(post_transition))
        .route("/test/state", post(post_test_state))
        .with_state(sessions)
}

// ── wire helpers ─────────────────────────────────────────────────────────────

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

// ── handlers ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct MachineQuery {
    machine: String,
}

async fn get_state(
    Query(q): Query<MachineQuery>,
    State(sessions): State<Sessions>,
) -> Response {
    let sessions = sessions.lock().unwrap();
    match sessions.get(&q.machine) {
        Some(inst) => msgpack(&inst.snapshot()),
        None => msgpack_err(StatusCode::NOT_FOUND, format!("machine '{}' not found", q.machine)),
    }
}

#[derive(Deserialize)]
struct TransitionRequest {
    machine: String,
    event: String,
    #[serde(default)]
    payload: Value,
}

async fn post_transition(State(sessions): State<Sessions>, body: Bytes) -> Response {
    let req: TransitionRequest = match rmp_serde::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return msgpack_err(StatusCode::BAD_REQUEST, e.to_string()),
    };

    let mut sessions = sessions.lock().unwrap();
    let inst = match sessions.get_mut(&req.machine) {
        Some(i) => i,
        None => return msgpack_err(StatusCode::NOT_FOUND, format!("machine '{}' not found", req.machine)),
    };

    match inst.send(&req.event, req.payload) {
        Ok(snap) => msgpack(&snap),
        Err(e) => msgpack_err(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

/// Inject an arbitrary snapshot — bypasses all transition logic.
/// JSON in / JSON out so it's trivially curl-able:
///
///   curl -X POST http://localhost:3000/test/state \
///        -H 'Content-Type: application/json' \
///        -d '{"machine_id":"counter","state":"error","context":{"count":99},"version":42}'
async fn post_test_state(
    State(sessions): State<Sessions>,
    Json(snap): Json<Snapshot>,
) -> Result<Json<Snapshot>, (StatusCode, String)> {
    let mut sessions = sessions.lock().unwrap();
    let inst = sessions
        .get_mut(&snap.machine_id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("machine '{}' not found", snap.machine_id)))?;

    inst.restore(snap)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    Ok(Json(inst.snapshot()))
}
