use foster_core::MachineBuilder;
use std::collections::HashMap;
use tower_http::services::ServeDir;

// Two independent machines sharing one page: theme (light/dark) and nav
// (contact dropdown open/closed). Previously this required folding both into
// one compound machine, because `validate_template` scanned the whole page
// against a single machine's vocabulary with no awareness of which
// `[fx-machine]` subtree an attribute belonged to. Fixed upstream in
// foster-core (`Machine::validate_in` now scopes to each machine's own
// `[fx-machine="{id}"]` subtree) — this is the real, two-machine version.
#[tokio::main]
async fn main() {
    let theme = MachineBuilder::new("theme", "light", serde_json::json!({}))
        .state("dark")
        .pass("light", "toggle_theme", "dark")
        .pass("dark", "toggle_theme", "light")
        .build();

    let nav = MachineBuilder::new("nav", "closed", serde_json::json!({}))
        .state("open")
        .pass("closed", "toggle_contact", "open")
        .pass("open", "toggle_contact", "closed")
        .pass("open", "close_contact", "closed")
        .template(include_str!("../static/index.html"))
        .build();

    // Play/pause + reset chrome for the Game of Life canvas below. The actual
    // simulation is a hand-rolled requestAnimationFrame loop over raw
    // wasm-bindgen/web-sys canvas code (see static/life.js) — Foster only
    // owns the button state, not the per-frame work. The loop reads this
    // machine's `data-fx-state` off the DOM each frame to know whether to
    // step, and watches `reset_nonce` (via `fx-text`) to know when to
    // reseed the grid. That's the actual integration boundary this example
    // exists to test: does a real canvas widget coexist with Foster's
    // server-authoritative chrome without a Rust-side binding between them.
    let life = MachineBuilder::new("life", "paused", serde_json::json!({ "reset_nonce": 0 }))
        .state("running")
        .pass("paused", "toggle_run", "running")
        .pass("running", "toggle_run", "paused")
        .on("paused", "reset", "paused", |ctx, _| {
            let n = ctx["reset_nonce"].as_i64().unwrap_or(0);
            Ok(serde_json::json!({ "reset_nonce": n + 1 }))
        })
        .on("running", "reset", "running", |ctx, _| {
            let n = ctx["reset_nonce"].as_i64().unwrap_or(0);
            Ok(serde_json::json!({ "reset_nonce": n + 1 }))
        })
        .build();

    let mut machines = HashMap::new();
    machines.insert("theme".to_string(), theme);
    machines.insert("nav".to_string(), nav);
    machines.insert("life".to_string(), life);

    let pkg_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg");
    let static_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

    let app = foster_server::router(machines)
        .nest_service("/pkg", ServeDir::new(pkg_dir))
        .fallback_service(ServeDir::new(static_dir));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3009").await.unwrap();
    println!("Foster jaydanhoward → http://localhost:3009");
    axum::serve(listener, app).await.unwrap();
}
