use foster_core::{MachineBuilder, MachineError};
use foster_server::router;
use serde_json::{json, Value};
use std::collections::HashMap;
use tower_http::services::ServeDir;

// fn-pointer reducers (not closures) so they are Send + Sync
fn to_calm(_ctx: Value, _: Value)       -> Result<Value, MachineError> { Ok(json!({ "description": "Still. Quiet. Present.", "intensity": 0.2 })) }
fn to_focused(_ctx: Value, _: Value)    -> Result<Value, MachineError> { Ok(json!({ "description": "Sharp. Clear. Directed.", "intensity": 0.5 })) }
fn to_energized(_ctx: Value, _: Value)  -> Result<Value, MachineError> { Ok(json!({ "description": "Alive. Bright. Flowing.", "intensity": 0.85 })) }
fn to_overwhelmed(_ctx: Value, _: Value)-> Result<Value, MachineError> { Ok(json!({ "description": "Scattered. Heavy. Much.", "intensity": 1.0 })) }

#[tokio::main]
async fn main() {
    let machine = MachineBuilder::new(
        "aura",
        "calm",
        json!({ "description": "Still. Quiet. Present.", "intensity": 0.2 }),
    )
    .state("focused")
    .state("energized")
    .state("overwhelmed")
    // complete graph: every state can transition to every other state
    .on("calm",       "focus",     "focused",    Some(to_focused))
    .on("calm",       "energize",  "energized",  Some(to_energized))
    .on("calm",       "overwhelm", "overwhelmed",Some(to_overwhelmed))
    .on("focused",    "calm",      "calm",        Some(to_calm))
    .on("focused",    "energize",  "energized",  Some(to_energized))
    .on("focused",    "overwhelm", "overwhelmed",Some(to_overwhelmed))
    .on("energized",  "calm",      "calm",        Some(to_calm))
    .on("energized",  "focus",     "focused",    Some(to_focused))
    .on("energized",  "overwhelm", "overwhelmed",Some(to_overwhelmed))
    .on("overwhelmed","calm",      "calm",        Some(to_calm))
    .on("overwhelmed","focus",     "focused",    Some(to_focused))
    .on("overwhelmed","energize",  "energized",  Some(to_energized))
    .build();

    let pkg_dir  = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg");
    let static_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

    let mut machines = HashMap::new();
    machines.insert("aura".to_string(), machine);

    let app = router(machines)
        .nest_service("/pkg", ServeDir::new(pkg_dir))
        .nest_service("/", ServeDir::new(static_dir));

    let addr = "0.0.0.0:3003";
    println!("aura  →  http://localhost:3003");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
