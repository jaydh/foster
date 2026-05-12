use foster_core::MachineBuilder;
use foster_server::router;
use serde_json::json;
use std::collections::HashMap;
use tower_http::services::ServeDir;

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
    .on("calm",       "focus",     "focused",     |_, _| Ok(json!({ "description": "Sharp. Clear. Directed.", "intensity": 0.5  })))
    .on("calm",       "energize",  "energized",   |_, _| Ok(json!({ "description": "Alive. Bright. Flowing.", "intensity": 0.85 })))
    .on("calm",       "overwhelm", "overwhelmed", |_, _| Ok(json!({ "description": "Scattered. Heavy. Much.", "intensity": 1.0  })))
    .on("focused",    "calm",      "calm",        |_, _| Ok(json!({ "description": "Still. Quiet. Present.", "intensity": 0.2  })))
    .on("focused",    "energize",  "energized",   |_, _| Ok(json!({ "description": "Alive. Bright. Flowing.", "intensity": 0.85 })))
    .on("focused",    "overwhelm", "overwhelmed", |_, _| Ok(json!({ "description": "Scattered. Heavy. Much.", "intensity": 1.0  })))
    .on("energized",  "calm",      "calm",        |_, _| Ok(json!({ "description": "Still. Quiet. Present.", "intensity": 0.2  })))
    .on("energized",  "focus",     "focused",     |_, _| Ok(json!({ "description": "Sharp. Clear. Directed.", "intensity": 0.5  })))
    .on("energized",  "overwhelm", "overwhelmed", |_, _| Ok(json!({ "description": "Scattered. Heavy. Much.", "intensity": 1.0  })))
    .on("overwhelmed","calm",      "calm",        |_, _| Ok(json!({ "description": "Still. Quiet. Present.", "intensity": 0.2  })))
    .on("overwhelmed","focus",     "focused",     |_, _| Ok(json!({ "description": "Sharp. Clear. Directed.", "intensity": 0.5  })))
    .on("overwhelmed","energize",  "energized",   |_, _| Ok(json!({ "description": "Alive. Bright. Flowing.", "intensity": 0.85 })))
    .template(include_str!("../static/index.html"))
    .build();

    let pkg_dir    = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg");
    let static_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

    let mut machines = HashMap::new();
    machines.insert("aura".to_string(), machine);

    // ServeDir at "/" serves CSS and other static assets; explicit GET / from the template
    // registered by router() takes priority for the root path.
    let app = router(machines)
        .nest_service("/pkg", ServeDir::new(pkg_dir))
        .nest_service("/", ServeDir::new(static_dir));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3003").await.unwrap();
    println!("Foster aura → http://localhost:3003");
    axum::serve(listener, app).await.unwrap();
}
