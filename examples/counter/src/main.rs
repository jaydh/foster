use axum::{response::Html, routing::get};
use foster_core::{MachineBuilder, MachineError};
use serde_json::{json, Value};
use std::collections::HashMap;
use tower_http::services::ServeDir;

// ── reducers ─────────────────────────────────────────────────────────────────
// Pure functions: (old_context, event_payload) → new_context.
// Kept at module scope so they're nameable in the machine definition.

fn increment(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let n = ctx["count"].as_i64().unwrap_or(0);
    Ok(json!({ "count": n + 1 }))
}

fn decrement(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let n = ctx["count"].as_i64().unwrap_or(0);
    Ok(json!({ "count": n - 1 }))
}

fn reset(_: Value, _: Value) -> Result<Value, MachineError> {
    Ok(json!({ "count": 0 }))
}

fn passthrough(ctx: Value, _: Value) -> Result<Value, MachineError> {
    Ok(ctx)
}

// ── machine definition ────────────────────────────────────────────────────────

fn counter_machine() -> std::sync::Arc<foster_core::Machine> {
    MachineBuilder::new("counter", "idle", json!({ "count": 0 }))
        // Explicit state declarations make the state space visible to tooling
        // (test generators, docs) even for states that are only transition targets.
        .state("error")
        // idle transitions
        .on("idle", "increment", "idle", Some(increment))
        .on("idle", "decrement", "idle", Some(decrement))
        .on("idle", "reset", "idle", Some(reset))
        .on("idle", "break_it", "error", Some(passthrough))
        // error transitions
        .on("error", "recover", "idle", Some(passthrough))
        .build()
}

// ── server ────────────────────────────────────────────────────────────────────

async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

#[tokio::main]
async fn main() {
    let mut machines = HashMap::new();
    machines.insert("counter".to_string(), counter_machine());

    // CARGO_MANIFEST_DIR is the examples/counter directory at compile time,
    // so pkg/ resolves correctly regardless of where `cargo run` is invoked from.
    let pkg_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/pkg");

    let app = foster_server::router(machines)
        .route("/", get(index))
        .nest_service("/pkg", ServeDir::new(pkg_dir));

    let addr = "0.0.0.0:3000";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();

    println!("Foster counter example");
    println!("  http://localhost:3000");
    println!();
    println!("Build the WASM client first if you haven't:");
    println!("  cd crates/foster-client");
    println!("  wasm-pack build --target web --out-dir ../../examples/counter/pkg");

    axum::serve(listener, app).await.unwrap();
}
