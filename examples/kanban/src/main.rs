use axum::{response::Html, routing::get};
use foster_core::{MachineBuilder, MachineError};
use serde_json::{json, Value};
use std::collections::HashMap;
use tower_http::services::ServeDir;

// ── helpers ───────────────────────────────────────────────────────────────────

fn next_id(tasks: &[Value]) -> String {
    let max = tasks
        .iter()
        .filter_map(|t| t["id"].as_str()?.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    (max + 1).to_string()
}

fn tasks_arr(ctx: &Value) -> Vec<Value> {
    ctx["tasks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

// ── reducers ──────────────────────────────────────────────────────────────────

/// viewing → start_create — clear the draft field.
fn begin_create(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let mut map = ctx.as_object().cloned().unwrap_or_default();
    map.insert("draft_title".into(), json!(""));
    map.insert("editing_id".into(), json!(""));
    Ok(Value::Object(map))
}

/// creating → save — add a new task using the draft title from the payload.
fn save_new_task(ctx: Value, payload: Value) -> Result<Value, MachineError> {
    let title = payload["draft_title"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Untitled")
        .trim()
        .to_string();

    let mut tasks = tasks_arr(&ctx);
    let id = next_id(&tasks);
    tasks.push(json!({ "id": id, "title": title, "column": "todo" }));

    let mut map = ctx.as_object().cloned().unwrap_or_default();
    map.insert("tasks".into(), Value::Array(tasks));
    map.insert("draft_title".into(), json!(""));
    Ok(Value::Object(map))
}

/// viewing → start_edit — set editing_id and pre-fill draft_title from the task.
fn begin_edit(ctx: Value, payload: Value) -> Result<Value, MachineError> {
    let id = payload["id"].as_str().unwrap_or("").to_string();
    let tasks = tasks_arr(&ctx);
    let title = tasks
        .iter()
        .find(|t| t["id"].as_str() == Some(&id))
        .and_then(|t| t["title"].as_str())
        .unwrap_or("")
        .to_string();

    let mut map = ctx.as_object().cloned().unwrap_or_default();
    map.insert("editing_id".into(), json!(id));
    map.insert("draft_title".into(), json!(title));
    Ok(Value::Object(map))
}

/// editing → save — update the task's title in the tasks array.
fn save_edit(ctx: Value, payload: Value) -> Result<Value, MachineError> {
    let id = ctx["editing_id"].as_str().unwrap_or("").to_string();
    let title = payload["draft_title"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Untitled")
        .trim()
        .to_string();

    let tasks: Vec<Value> = tasks_arr(&ctx)
        .into_iter()
        .map(|mut t| {
            if t["id"].as_str() == Some(&id) {
                t["title"] = json!(title);
            }
            t
        })
        .collect();

    let mut map = ctx.as_object().cloned().unwrap_or_default();
    map.insert("tasks".into(), Value::Array(tasks));
    map.insert("editing_id".into(), json!(""));
    map.insert("draft_title".into(), json!(""));
    Ok(Value::Object(map))
}

/// viewing → start_delete — record which task is pending deletion.
fn begin_delete(ctx: Value, payload: Value) -> Result<Value, MachineError> {
    let id = payload["id"].as_str().unwrap_or("").to_string();
    let mut map = ctx.as_object().cloned().unwrap_or_default();
    map.insert("confirm_id".into(), json!(id));
    Ok(Value::Object(map))
}

/// confirming_delete → confirm — remove the task.
fn confirm_delete(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let id = ctx["confirm_id"].as_str().unwrap_or("").to_string();
    let tasks: Vec<Value> = tasks_arr(&ctx)
        .into_iter()
        .filter(|t| t["id"].as_str() != Some(&id))
        .collect();

    let mut map = ctx.as_object().cloned().unwrap_or_default();
    map.insert("tasks".into(), Value::Array(tasks));
    map.insert("confirm_id".into(), json!(""));
    Ok(Value::Object(map))
}

/// Generic column move: set task.column to the value in payload["column"].
fn move_task(ctx: Value, payload: Value) -> Result<Value, MachineError> {
    let id     = payload["id"].as_str().unwrap_or("").to_string();
    let column = payload["column"].as_str().unwrap_or("todo").to_string();

    let tasks: Vec<Value> = tasks_arr(&ctx)
        .into_iter()
        .map(|mut t| {
            if t["id"].as_str() == Some(&id) {
                t["column"] = json!(column);
            }
            t
        })
        .collect();

    let mut map = ctx.as_object().cloned().unwrap_or_default();
    map.insert("tasks".into(), Value::Array(tasks));
    Ok(Value::Object(map))
}

fn cancel(ctx: Value, _: Value) -> Result<Value, MachineError> {
    let mut map = ctx.as_object().cloned().unwrap_or_default();
    map.insert("draft_title".into(), json!(""));
    map.insert("editing_id".into(), json!(""));
    map.insert("confirm_id".into(), json!(""));
    Ok(Value::Object(map))
}

// ── machine ───────────────────────────────────────────────────────────────────

pub fn kanban_machine() -> std::sync::Arc<foster_core::Machine> {
    MachineBuilder::new(
        "kanban",
        "viewing",
        json!({
            "tasks": [
                { "id": "1", "title": "Design state model",   "column": "done"        },
                { "id": "2", "title": "Build WASM client",    "column": "in_progress" },
                { "id": "3", "title": "Write Playwright tests","column": "todo"        }
            ],
            "draft_title": "",
            "editing_id":  "",
            "confirm_id":  ""
        }),
    )
    .state("creating")
    .state("editing")
    .state("confirming_delete")
    // viewing transitions
    .on("viewing", "start_create", "creating",          Some(begin_create))
    .on("viewing", "start_edit",   "editing",           Some(begin_edit))
    .on("viewing", "start_delete", "confirming_delete", Some(begin_delete))
    .on("viewing", "move_task",    "viewing",           Some(move_task))
    // creating transitions
    .on("creating", "save",   "viewing", Some(save_new_task))
    .on("creating", "cancel", "viewing", Some(cancel))
    // editing transitions
    .on("editing", "save",   "viewing", Some(save_edit))
    .on("editing", "cancel", "viewing", Some(cancel))
    // confirming_delete transitions
    .on("confirming_delete", "confirm", "viewing", Some(confirm_delete))
    .on("confirming_delete", "cancel",  "viewing", Some(cancel))
    .build()
}

// ── server ────────────────────────────────────────────────────────────────────

async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

#[tokio::main]
async fn main() {
    let mut machines = HashMap::new();
    machines.insert("kanban".to_string(), kanban_machine());

    let pkg_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg");

    let app = foster_server::router(machines)
        .route("/", get(index))
        .nest_service("/pkg", ServeDir::new(pkg_dir));

    let addr = "0.0.0.0:3002";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("Foster kanban example → http://localhost:3002");
    axum::serve(listener, app).await.unwrap();
}
