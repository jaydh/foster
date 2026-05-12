use foster_core::{MachineBuilder, MachineError};
use serde_json::{json, Value};
use std::collections::HashMap;
use tower_http::services::ServeDir;

// ── reducers ──────────────────────────────────────────────────────────────────

fn load_track(_: Value, _: Value) -> Result<Value, MachineError> {
    Ok(json!({ "title": "Rust Never Sleeps", "artist": "The Crabs", "position": 0, "duration": 213, "error": "" }))
}

fn seek_forward(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let pos = ctx["position"].as_i64().unwrap_or(0);
    let dur = ctx["duration"].as_i64().unwrap_or(0);
    let mut m = ctx.as_object().cloned().unwrap_or_default();
    m.insert("position".into(), json!((pos + 10).min(dur)));
    Ok(Value::Object(m))
}

fn seek_back(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let pos = ctx["position"].as_i64().unwrap_or(0);
    let mut m = ctx.as_object().cloned().unwrap_or_default();
    m.insert("position".into(), json!((pos - 10).max(0)));
    Ok(Value::Object(m))
}

fn set_ended(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let dur = ctx["duration"].as_i64().unwrap_or(0);
    let mut m = ctx.as_object().cloned().unwrap_or_default();
    m.insert("position".into(), json!(dur));
    Ok(Value::Object(m))
}

fn reset_position(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let mut m = ctx.as_object().cloned().unwrap_or_default();
    m.insert("position".into(), json!(0));
    Ok(Value::Object(m))
}

fn set_error(ctx: Value, payload: Value) -> Result<Value, MachineError> {
    let msg = payload["message"].as_str().unwrap_or("Playback failed").to_string();
    let mut m = ctx.as_object().cloned().unwrap_or_default();
    m.insert("error".into(), json!(msg));
    Ok(Value::Object(m))
}

fn clear_error(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let mut m = ctx.as_object().cloned().unwrap_or_default();
    m.insert("error".into(), json!(""));
    Ok(Value::Object(m))
}

// ── machine + server ──────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let machine = MachineBuilder::new(
        "player",
        "idle",
        json!({ "title": "", "artist": "", "position": 0, "duration": 0, "error": "" }),
    )
    .state("loading")
    .state("playing")
    .state("paused")
    .state("ended")
    .state("error")
    .on("idle",    "load",       "loading", load_track)
    .pass("loading", "ready",    "playing")
    .on("loading", "fail",       "error",   set_error)
    .pass("playing", "pause",    "paused")
    .on("playing", "forward_10", "playing", seek_forward)
    .on("playing", "back_10",    "playing", seek_back)
    .on("playing", "end",        "ended",   set_ended)
    .pass("paused", "play",      "playing")
    .on("paused",  "forward_10", "paused",  seek_forward)
    .on("paused",  "back_10",    "paused",  seek_back)
    .on("ended",   "replay",     "playing", reset_position)
    .on("ended",   "load",       "loading", load_track)
    .on("error",   "retry",      "loading", clear_error)
    .pass("error", "dismiss",    "idle")
    .template(include_str!("../static/index.html"))
    .build();

    let mut machines = HashMap::new();
    machines.insert("player".to_string(), machine);

    let pkg_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg");
    let app = foster_server::router(machines)
        .nest_service("/pkg", ServeDir::new(pkg_dir));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3001").await.unwrap();
    println!("Foster player → http://localhost:3001");
    axum::serve(listener, app).await.unwrap();
}
