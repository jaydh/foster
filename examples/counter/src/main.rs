use foster_core::{html, page, MachineBuilder};
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
        .template(page("Foster • Counter", include_str!("../static/style.css"), html! {
            h1 { "Foster UI · Counter" }
            div[machine="counter"] {
                p[class="meta"] { "state: " span[class="badge", state_label] { "…" } }
                div[show="idle"] {
                    div[class="count-display"] { span[text="count"] { "0" } }
                    div[class="controls"] {
                        button[on="click->decrement"] { "−" }
                        button[on="click->increment"] { "+" }
                        button[on="click->reset"] { "reset" }
                        button[class="btn-danger", on="click->break_it"] { "break" }
                    }
                }
                div[show="error", class="error-box"] {
                    p { "Machine entered error state. Count was: "
                        span[class="error-count", text="count"] { "—" }
                    }
                    button[class="btn-safe", on="click->recover"] { "recover" }
                }
            }
            hr {}
            details {
                summary { "snapshot" }
                pre[id="debug-snapshot"] { "{}" }
            }
        }))
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
