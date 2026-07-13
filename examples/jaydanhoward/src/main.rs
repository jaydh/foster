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

    let mut machines = HashMap::new();
    machines.insert("theme".to_string(), theme);
    machines.insert("nav".to_string(), nav);

    let pkg_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg");
    let static_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

    let app = foster_server::router(machines)
        .nest_service("/pkg", ServeDir::new(pkg_dir))
        .fallback_service(ServeDir::new(static_dir));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3009").await.unwrap();
    println!("Foster jaydanhoward → http://localhost:3009");
    axum::serve(listener, app).await.unwrap();
}
