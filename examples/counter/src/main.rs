use foster_core::MachineBuilder;
use serde_json::json;
use std::collections::HashMap;
use tower_http::services::ServeDir;

#[tokio::main]
async fn main() {
    let machine = MachineBuilder::new("counter", "idle", json!({ "count": 0 }))
        .state("error")
        .on("idle", "increment", "idle", |ctx, _| Ok(json!({ "count": ctx["count"].as_i64().unwrap_or(0) + 1 })))
        .on("idle", "decrement", "idle", |ctx, _| Ok(json!({ "count": ctx["count"].as_i64().unwrap_or(0) - 1 })))
        .on("idle", "reset",     "idle", |_, _|   Ok(json!({ "count": 0 })))
        .pass("idle",  "break_it", "error")
        .pass("error", "recover",  "idle")
        .template(include_str!("../static/index.html"))
        .build();

    let mut machines = HashMap::new();
    machines.insert("counter".to_string(), machine);

    let pkg_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg");
    let app = foster_server::router(machines)
        .nest_service("/pkg", ServeDir::new(pkg_dir));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("Foster counter → http://localhost:3000");
    axum::serve(listener, app).await.unwrap();
}
