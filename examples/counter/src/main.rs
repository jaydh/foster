//! The smallest Foster app: a counter that can break and recover. Also the
//! repo's smoke test — `npx playwright test` here runs tests/counter.spec.ts
//! against it.

use foster_core::{page, MachineBuilder};
use serde_json::json;
use std::collections::HashMap;
use tower_http::services::ServeDir;

const BODY: &str = r#"
<h1>Foster UI · Counter</h1>
<div fx-machine="counter">
  <div fx-show="idle">
    <div class="count-display"><span fx-text="count">0</span></div>
    <div class="controls">
      <button fx-on="click->decrement">−</button>
      <button fx-on="click->increment">+</button>
      <button fx-on="click->reset">reset</button>
      <button class="btn-danger" fx-on="click->break_it">break</button>
    </div>
  </div>
  <div fx-show="error" class="error-box">
    <p>Machine entered error state. Count was: <span class="error-count" fx-text="count">—</span></p>
    <button class="btn-safe" fx-on="click->recover">recover</button>
  </div>
</div>
"#;

#[tokio::main]
async fn main() {
    let count = |ctx: &serde_json::Value| ctx["count"].as_i64().unwrap_or(0);
    let machine = MachineBuilder::new("counter", "idle", json!({ "count": 0 }))
        .state("error")
        .on("idle", "increment", "idle", move |ctx, _| Ok(json!({ "count": count(&ctx) + 1 })))
        .on("idle", "decrement", "idle", move |ctx, _| Ok(json!({ "count": count(&ctx) - 1 })))
        .on("idle", "reset", "idle", |_, _| Ok(json!({ "count": 0 })))
        .pass("idle", "break_it", "error")
        .pass("error", "recover", "idle")
        .template(page("Foster • Counter", include_str!("../static/style.css"), BODY))
        .build();

    let machines = HashMap::from([("counter".to_string(), machine)]);
    let pkg_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg");
    let app = foster_server::router(machines).nest_service("/pkg", ServeDir::new(pkg_dir));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("Foster counter → http://localhost:3000");
    axum::serve(listener, app).await.unwrap();
}
