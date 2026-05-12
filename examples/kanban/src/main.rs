use foster_core::{html, page, MachineBuilder, MachineError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use tower_http::services::ServeDir;

// ── typed context ─────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Default)]
struct Task {
    id: String,
    title: String,
    column: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct KanbanCtx {
    tasks: Vec<Task>,
    draft_title: String,
    editing_id: String,
    confirm_id: String,
}

fn next_id(tasks: &[Task]) -> String {
    let max = tasks.iter().filter_map(|t| t.id.parse::<u64>().ok()).max().unwrap_or(0);
    (max + 1).to_string()
}

// ── reducers ──────────────────────────────────────────────────────────────────

fn begin_create(mut ctx: KanbanCtx, _: Value) -> Result<KanbanCtx, MachineError> {
    ctx.draft_title = String::new();
    ctx.editing_id  = String::new();
    Ok(ctx)
}

fn save_new_task(mut ctx: KanbanCtx, payload: Value) -> Result<KanbanCtx, MachineError> {
    let title = payload["draft_title"].as_str().unwrap_or("Untitled").trim().to_string();
    let id = next_id(&ctx.tasks);
    ctx.tasks.push(Task { id, title, column: "todo".into() });
    ctx.draft_title = String::new();
    Ok(ctx)
}

fn begin_edit(mut ctx: KanbanCtx, payload: Value) -> Result<KanbanCtx, MachineError> {
    let id = payload["id"].as_str().unwrap_or("").to_string();
    ctx.draft_title = ctx.tasks.iter().find(|t| t.id == id)
        .map(|t| t.title.clone()).unwrap_or_default();
    ctx.editing_id = id;
    Ok(ctx)
}

fn save_edit(mut ctx: KanbanCtx, payload: Value) -> Result<KanbanCtx, MachineError> {
    let title = payload["draft_title"].as_str().unwrap_or("Untitled").trim().to_string();
    for task in &mut ctx.tasks {
        if task.id == ctx.editing_id { task.title = title.clone(); }
    }
    ctx.editing_id  = String::new();
    ctx.draft_title = String::new();
    Ok(ctx)
}

fn begin_delete(mut ctx: KanbanCtx, payload: Value) -> Result<KanbanCtx, MachineError> {
    ctx.confirm_id = payload["id"].as_str().unwrap_or("").to_string();
    Ok(ctx)
}

fn confirm_delete(mut ctx: KanbanCtx, _: Value) -> Result<KanbanCtx, MachineError> {
    ctx.tasks.retain(|t| t.id != ctx.confirm_id);
    ctx.confirm_id = String::new();
    Ok(ctx)
}

fn move_task(mut ctx: KanbanCtx, payload: Value) -> Result<KanbanCtx, MachineError> {
    let id     = payload["id"].as_str().unwrap_or("").to_string();
    let column = payload["column"].as_str().unwrap_or("todo").to_string();
    for task in &mut ctx.tasks { if task.id == id { task.column = column.clone(); } }
    Ok(ctx)
}

fn cancel(mut ctx: KanbanCtx, _: Value) -> Result<KanbanCtx, MachineError> {
    ctx.draft_title = String::new();
    ctx.editing_id  = String::new();
    ctx.confirm_id  = String::new();
    Ok(ctx)
}

// ── machine + server ──────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let machine = MachineBuilder::new("kanban", "viewing", json!({
        "tasks": [
            { "id": "1", "title": "Design state model",    "column": "done"        },
            { "id": "2", "title": "Build WASM client",     "column": "in_progress" },
            { "id": "3", "title": "Write Playwright tests", "column": "todo"        }
        ],
        "draft_title": "",
        "editing_id":  "",
        "confirm_id":  ""
    }))
    .state("creating")
    .state("editing")
    .state("confirming_delete")
    .typed_on("viewing",           "start_create", "creating",          begin_create)
    .typed_on("viewing",           "start_edit",   "editing",           begin_edit)
    .typed_on("viewing",           "start_delete", "confirming_delete", begin_delete)
    .typed_on("viewing",           "move_task",    "viewing",           move_task)
    .typed_on("creating",          "save",         "viewing",           save_new_task)
    .typed_on("creating",          "cancel",       "viewing",           cancel)
    .typed_on("editing",           "save",         "viewing",           save_edit)
    .typed_on("editing",           "cancel",       "viewing",           cancel)
    .typed_on("confirming_delete", "confirm",      "viewing",           confirm_delete)
    .typed_on("confirming_delete", "cancel",       "viewing",           cancel)
    .template(page("Foster · Kanban", include_str!("../static/style.css"), html! {
        div[machine="kanban"] {
            h1 { "Foster Kanban · state: " span[class="badge", state_label] { "…" } }
            div[class="toolbar", show="viewing"] {
                button[class="btn-add", on="click->start_create"] { "+ New task" }
            }
            div[class="board", show="viewing"] {
                div {
                    div[class="col-header"] { "Todo" }
                    div[each="tasks", filter=r#"{"column":"todo"}"#] {
                        div[class="task-card", data_template, style="display:none"] {
                            div[class="task-title", field="title"] { "—" }
                            div[class="card-actions"] {
                                button[class="btn", on="click->start_edit"] { "edit" }
                                button[class="btn btn-danger", on="click->start_delete"] { "del" }
                                button[class="btn btn-safe", on="click->move_task", payload=r#"{"column":"in_progress"}"#] { "→ IP" }
                            }
                        }
                    }
                }
                div {
                    div[class="col-header"] { "In Progress" }
                    div[each="tasks", filter=r#"{"column":"in_progress"}"#] {
                        div[class="task-card", data_template, style="display:none"] {
                            div[class="task-title", field="title"] { "—" }
                            div[class="card-actions"] {
                                button[class="btn", on="click->start_edit"] { "edit" }
                                button[class="btn btn-danger", on="click->start_delete"] { "del" }
                                button[class="btn btn-safe", on="click->move_task", payload=r#"{"column":"done"}"#] { "→ Done" }
                                button[class="btn", on="click->move_task", payload=r#"{"column":"todo"}"#] { "← Todo" }
                            }
                        }
                    }
                }
                div {
                    div[class="col-header"] { "Done" }
                    div[each="tasks", filter=r#"{"column":"done"}"#] {
                        div[class="task-card", data_template, style="display:none"] {
                            div[class="task-title", field="title"] { "—" }
                            div[class="card-actions"] {
                                button[class="btn btn-danger", on="click->start_delete"] { "del" }
                                button[class="btn", on="click->move_task", payload=r#"{"column":"in_progress"}"#] { "← IP" }
                            }
                        }
                    }
                }
            }
            div[class="modal", show="creating"] {
                div[class="modal-box"] {
                    div[class="modal-title"] { "New task" }
                    input[type="text", placeholder="Task title…", collect="draft_title", autofocus]
                    div[class="modal-actions"] {
                        button[class="btn", on="click->cancel"] { "Cancel" }
                        button[class="btn btn-safe", on="click->save"] { "Save" }
                    }
                }
            }
            div[class="modal", show="editing"] {
                div[class="modal-box"] {
                    div[class="modal-title"] { "Edit task" }
                    input[type="text", placeholder="Task title…", value="draft_title", collect="draft_title"]
                    div[class="modal-actions"] {
                        button[class="btn", on="click->cancel"] { "Cancel" }
                        button[class="btn btn-safe", on="click->save"] { "Save" }
                    }
                }
            }
            div[class="modal", show="confirming_delete"] {
                div[class="modal-box"] {
                    div[class="modal-title"] { "Delete task?" }
                    p[style="color:#888;font-size:0.82rem;margin-bottom:1rem"] { "This cannot be undone." }
                    div[class="modal-actions"] {
                        button[class="btn", on="click->cancel"] { "Cancel" }
                        button[class="btn btn-danger", on="click->confirm"] { "Delete" }
                    }
                }
            }
        }
        details {
            summary { "snapshot" }
            pre[id="debug-snapshot"] { "{}" }
        }
    }))
    .build();

    let mut machines = HashMap::new();
    machines.insert("kanban".to_string(), machine);

    let pkg_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg");
    let app = foster_server::router(machines)
        .nest_service("/pkg", ServeDir::new(pkg_dir));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3002").await.unwrap();
    println!("Foster kanban → http://localhost:3002");
    axum::serve(listener, app).await.unwrap();
}
