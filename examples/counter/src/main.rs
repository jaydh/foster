use foster_core::{html, machine_graph, page, MachineBuilder};
use serde_json::json;
use std::collections::HashMap;
use tower_http::services::ServeDir;

// Declares CounterState and CounterEvent enums; validates graph at compile time.
machine_graph! {
    id: "counter",
    initial: "idle",
    states: ["idle", "error"],
    transitions: [
        ("idle",  "increment", "idle"),
        ("idle",  "decrement", "idle"),
        ("idle",  "reset",     "idle"),
        ("idle",  "break_it",  "error"),
        ("error", "recover",   "idle"),
    ]
}

#[tokio::main]
async fn main() {
    let machine = MachineBuilder::new(
            "counter",
            CounterState::Idle.as_str(),
            json!({ "count": 0 }),
        )
        .state(CounterState::Error.as_str())
        .on(CounterState::Idle.as_str(), CounterEvent::Increment.as_str(), CounterState::Idle.as_str(),
            |ctx, _| Ok(json!({ "count": ctx["count"].as_i64().unwrap_or(0) + 1 })))
        .on(CounterState::Idle.as_str(), CounterEvent::Decrement.as_str(), CounterState::Idle.as_str(),
            |ctx, _| Ok(json!({ "count": ctx["count"].as_i64().unwrap_or(0) - 1 })))
        .on(CounterState::Idle.as_str(), CounterEvent::Reset.as_str(), CounterState::Idle.as_str(),
            |_, _| Ok(json!({ "count": 0 })))
        .pass(CounterState::Idle.as_str(),  CounterEvent::BreakIt.as_str(),  CounterState::Error.as_str())
        .pass(CounterState::Error.as_str(), CounterEvent::Recover.as_str(), CounterState::Idle.as_str())
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
