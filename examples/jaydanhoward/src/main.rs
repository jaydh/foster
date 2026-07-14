mod cluster;
mod request_trace;
mod screening;
mod visitors;

use axum::routing::{get, post};
use axum::Router;
use foster_core::MachineBuilder;
use std::collections::HashMap;
use std::net::SocketAddr;
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
    // kube's rustls-tls feature needs an explicit process-level crypto
    // provider — same fix the real jaydanhoward site's main.rs applies.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

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

    // Satellite tracker: same pattern as "life" (Foster owns run/pause + the
    // per-ring visibility toggles, a hand-rolled canvas loop in
    // static/satellites.js does the actual per-frame orbital math and
    // drawing), but heavier — continuous animation of many points at once,
    // not a coarse simulation step every ~90ms. Tests whether the
    // DOM-attribute integration boundary (data-fx-state + fx-if markers)
    // still holds up at real animation frame rate.
    fn toggle_bool(key: &'static str) -> impl Fn(serde_json::Value, serde_json::Value) -> Result<serde_json::Value, foster_core::MachineError> + Clone {
        move |mut ctx, _payload| {
            let v = ctx.get(key).and_then(|v| v.as_bool()).unwrap_or(true);
            ctx[key] = serde_json::json!(!v);
            Ok(ctx)
        }
    }

    let satellites = MachineBuilder::new(
        "satellites",
        "running",
        serde_json::json!({ "show_leo": true, "show_meo": true, "show_geo": true }),
    )
    .state("paused")
    .pass("running", "toggle_run", "paused")
    .pass("paused", "toggle_run", "running")
    .on("running", "toggle_leo", "running", toggle_bool("show_leo"))
    .on("paused", "toggle_leo", "paused", toggle_bool("show_leo"))
    .on("running", "toggle_meo", "running", toggle_bool("show_meo"))
    .on("paused", "toggle_meo", "paused", toggle_bool("show_meo"))
    .on("running", "toggle_geo", "running", toggle_bool("show_geo"))
    .on("paused", "toggle_geo", "paused", toggle_bool("show_geo"))
    .build();

    // Conjunction-screening analog. Foster's role here is deliberately tiny —
    // just remembering that the button has been clicked at least once, so the
    // label can change from "Screen" to "Screen again". The actual status
    // (running/complete) and the event list come from an independent
    // setInterval poll against /api/screening (see screening.rs and
    // static/screening.js), a background job that finishes on its own
    // without any further client request — same shape as the real site's
    // conjunction screening. Both this machine's SSE connection and the
    // widget's own poll loop run concurrently on the same page.
    let conjunction = MachineBuilder::new("conjunction", "idle", serde_json::json!({}))
        .state("started")
        .pass("idle", "start_screening", "started")
        .pass("started", "start_screening", "started")
        .build();

    // Real GitOps (Flux) status + backup Job status via the kube crate
    // against the actual homelab cluster (see cluster.rs) — deliberately
    // not the Prometheus-backed CPU/mem/disk/Ceph panel from the real site's
    // cluster_stats.rs, since Prometheus's NodePort isn't reachable from
    // this machine (LAN-only, we're on Tailscale). "Refresh" re-queries the
    // cluster synchronously inside the reducer (via block_in_place since
    // Foster's reducers are plain sync Fn, no async support) — acceptable
    // for a quick API read, unlike the background-job pattern used by life/
    // satellites/conjunction.
    let cluster = MachineBuilder::new("cluster", "loaded", cluster::fetch_cluster_status())
        .on("loaded", "refresh", "loaded", |_ctx, _payload| {
            Ok(cluster::fetch_cluster_status())
        })
        .build();

    // Real visitor logging against a real (local, throwaway) Postgres — see
    // visitors.rs. Every non-static request gets logged by a global axum
    // middleware layer (below), independent of Foster entirely; the
    // "visitors" machine here just displays the accumulated real rows,
    // same refresh-reducer pattern as "cluster".
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:foster@localhost:5433/jaydanhoward".to_string());
    let pg_pool = visitors::create_pool(&database_url)
        .await
        .expect("Failed to connect to Postgres — is the local dev Postgres running? See migrations/0001_create_visitors.sql");

    let visitors_machine = {
        let pool_for_reducer = pg_pool.clone();
        MachineBuilder::new(
            "visitors",
            "loaded",
            visitors::fetch_visitor_stats(&pg_pool),
        )
        .on("loaded", "refresh", "loaded", move |_ctx, _payload| {
            Ok(visitors::fetch_visitor_stats(&pool_for_reducer))
        })
        .build()
    };

    let mut machines = HashMap::new();
    machines.insert("theme".to_string(), theme);
    machines.insert("nav".to_string(), nav);
    machines.insert("life".to_string(), life);
    machines.insert("satellites".to_string(), satellites);
    machines.insert("conjunction".to_string(), conjunction);
    machines.insert("cluster".to_string(), cluster);
    machines.insert("visitors".to_string(), visitors_machine);

    let pkg_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg");
    let static_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

    let screening_router = Router::new()
        .route("/api/screening", get(screening::get_screening))
        .route("/api/screening/start", post(screening::start_screening))
        .with_state(screening::initial_state());

    // Real per-request data (see request_trace.rs) — needs the client's real
    // socket address, which requires opting into ConnectInfo below.
    let trace_router: Router = Router::new()
        .route("/api/request-trace", get(request_trace::get_request_trace));

    let app = foster_server::router(machines)
        .merge(screening_router)
        .merge(trace_router)
        .nest_service("/pkg", ServeDir::new(pkg_dir))
        .fallback_service(ServeDir::new(static_dir))
        .layer(axum::middleware::from_fn_with_state(
            pg_pool,
            visitors::visitor_logger,
        ));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3009").await.unwrap();
    println!("Foster jaydanhoward → http://localhost:3009");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}
