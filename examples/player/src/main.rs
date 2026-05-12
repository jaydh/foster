use axum::{response::Html, routing::get};
use foster_core::{MachineBuilder, MachineError};
use serde_json::{json, Value};
use std::collections::HashMap;
use tower_http::services::ServeDir;

// ── reducers ──────────────────────────────────────────────────────────────────

fn load_track(_: Value, _: Value) -> Result<Value, MachineError> {
    // In a real app the payload would carry a track ID and the server would look
    // it up.  For the PoC we use a fixed track so the state machine stays the focus.
    Ok(json!({
        "title":    "Rust Never Sleeps",
        "artist":   "The Crabs",
        "position": 0,
        "duration": 213,
        "error":    ""
    }))
}

fn seek_forward(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let pos = ctx["position"].as_i64().unwrap_or(0);
    let dur = ctx["duration"].as_i64().unwrap_or(0);
    let mut map = ctx.as_object().cloned().unwrap_or_default();
    map.insert("position".into(), json!((pos + 10).min(dur)));
    Ok(Value::Object(map))
}

fn seek_back(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let pos = ctx["position"].as_i64().unwrap_or(0);
    let mut map = ctx.as_object().cloned().unwrap_or_default();
    map.insert("position".into(), json!((pos - 10).max(0)));
    Ok(Value::Object(map))
}

fn set_ended(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let dur = ctx["duration"].as_i64().unwrap_or(0);
    let mut map = ctx.as_object().cloned().unwrap_or_default();
    map.insert("position".into(), json!(dur));
    Ok(Value::Object(map))
}

fn reset_position(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let mut map = ctx.as_object().cloned().unwrap_or_default();
    map.insert("position".into(), json!(0));
    Ok(Value::Object(map))
}

fn set_error(ctx: Value, payload: Value) -> Result<Value, MachineError> {
    let msg = payload["message"]
        .as_str()
        .unwrap_or("Playback failed")
        .to_string();
    let mut map = ctx.as_object().cloned().unwrap_or_default();
    map.insert("error".into(), json!(msg));
    Ok(Value::Object(map))
}

fn clear_error(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let mut map = ctx.as_object().cloned().unwrap_or_default();
    map.insert("error".into(), json!(""));
    Ok(Value::Object(map))
}

// ── machine ───────────────────────────────────────────────────────────────────

pub fn player_machine() -> std::sync::Arc<foster_core::Machine> {
    MachineBuilder::new(
        "player",
        "idle",
        json!({ "title": "", "artist": "", "position": 0, "duration": 0, "error": "" }),
    )
    .state("loading")
    .state("playing")
    .state("paused")
    .state("ended")
    .state("error")
    // idle
    .on("idle", "load", "loading", Some(load_track))
    // loading
    .on("loading", "ready", "playing", None)
    .on("loading", "fail", "error", Some(set_error))
    // playing ↔ paused (symmetric seek; separate events so Playwright can cover both)
    .on("playing", "pause", "paused", None)
    .on("playing", "forward_10", "playing", Some(seek_forward))
    .on("playing", "back_10", "playing", Some(seek_back))
    .on("playing", "end", "ended", Some(set_ended))
    .on("paused", "play", "playing", None)
    .on("paused", "forward_10", "paused", Some(seek_forward))
    .on("paused", "back_10", "paused", Some(seek_back))
    // ended
    .on("ended", "replay", "playing", Some(reset_position))
    .on("ended", "load", "loading", Some(load_track))
    // error
    .on("error", "retry", "loading", Some(clear_error))
    .on("error", "dismiss", "idle", None)
    .build()
}

// ── server ────────────────────────────────────────────────────────────────────

async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

#[tokio::main]
async fn main() {
    let mut machines = HashMap::new();
    machines.insert("player".to_string(), player_machine());

    let pkg_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg");

    let app = foster_server::router(machines)
        .route("/", get(index))
        .nest_service("/pkg", ServeDir::new(pkg_dir));

    let addr = "0.0.0.0:3001";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("Foster player example → http://localhost:3001");
    axum::serve(listener, app).await.unwrap();
}
