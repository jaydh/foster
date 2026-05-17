use foster_core::{html, page, MachineBuilder, MachineError};
use foster_server::{router_with, RedisStore, RedisPubSub};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tower_http::services::ServeDir;

// ── data model ────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Default)]
struct PollOption {
    id:          String,
    label:       String,
    votes:       u32,
    percent:     u32,
    percent_str: String,   // "62%" — displayed inside the vote bar
    rank:        String,   // "1st", "2nd", etc. (set when poll is closed)
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct PollCtx {
    question:    String,
    options:     Vec<PollOption>,
    total_votes: u32,
    winner:      String,
    winner_votes: u32,
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn recompute_percents(options: &mut Vec<PollOption>, total: u32) {
    for opt in options.iter_mut() {
        let pct = if total > 0 { opt.votes * 100 / total } else { 0 };
        opt.percent     = pct;
        opt.percent_str = format!("{}%", pct);
    }
}

fn opt(id: &str, label: &str, votes: u32) -> PollOption {
    PollOption {
        id:          id.to_string(),
        label:       label.to_string(),
        votes,
        percent:     0,
        percent_str: "0%".to_string(),
        rank:        String::new(),
    }
}

fn seed_ctx() -> PollCtx {
    let mut options = vec![
        opt("web",   "Web Applications",  3),
        opt("api",   "Backend APIs",       5),
        opt("proto", "Rapid Prototyping", 2),
        opt("game",  "Games & Simulations",1),
    ];
    let total = options.iter().map(|o| o.votes).sum();
    recompute_percents(&mut options, total);
    PollCtx {
        question:     "What's your primary use case for Foster?".to_string(),
        options,
        total_votes:  total,
        winner:       String::new(),
        winner_votes: 0,
    }
}

// ── reducers ──────────────────────────────────────────────────────────────────

fn vote(mut ctx: PollCtx, payload: Value) -> Result<PollCtx, MachineError> {
    let id = payload["id"].as_str().unwrap_or("").to_string();
    if let Some(opt) = ctx.options.iter_mut().find(|o| o.id == id) {
        opt.votes += 1;
    }
    ctx.total_votes = ctx.options.iter().map(|o| o.votes).sum();
    recompute_percents(&mut ctx.options, ctx.total_votes);
    Ok(ctx)
}

fn close_poll(mut ctx: PollCtx, _: Value) -> Result<PollCtx, MachineError> {
    let mut sorted = ctx.options.clone();
    sorted.sort_by(|a, b| b.votes.cmp(&a.votes));

    let ranks = ["1st", "2nd", "3rd", "4th"];
    for (i, s) in sorted.iter().enumerate() {
        if let Some(opt) = ctx.options.iter_mut().find(|o| o.id == s.id) {
            opt.rank = ranks.get(i).unwrap_or(&"").to_string();
        }
    }

    if let Some(winner) = sorted.first() {
        ctx.winner       = winner.label.clone();
        ctx.winner_votes = winner.votes;
    }
    Ok(ctx)
}

fn reset_poll(_: PollCtx, _: Value) -> Result<PollCtx, MachineError> {
    Ok(seed_ctx())
}

// ── main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let redis_url = std::env::var("REDIS_URL")
        .unwrap_or_else(|_| "redis://127.0.0.1/".to_string());

    let store  = RedisStore::new(&redis_url)
        .expect("Redis connection failed — is Redis running? Try: docker compose up -d redis");
    let pubsub = RedisPubSub::new(&redis_url)
        .expect("Redis pub/sub failed");

    let ctx      = seed_ctx();
    let ctx_json = serde_json::to_value(&ctx).unwrap();

    let machine = MachineBuilder::new("poll", "open", ctx_json)
        .state("closed")
        .typed_on("open",   "vote",       "open",   vote)
        .typed_on("open",   "close_poll", "closed", close_poll)
        .typed_on("closed", "reset",      "open",   reset_poll)
        .template(page("Collab Poll", include_str!("../static/style.css"), html! {

            div[class="poll-page"] {
                div[class="poll-container"] {

                    // ── header ────────────────────────────────────────────────
                    div[class="poll-header"] {
                        div[class="poll-live-badge"] {
                            span[class="live-dot"] {}
                            " Live"
                        }
                        h1[class="poll-title"] { "Real-time Collaboration Demo" }
                        p[class="poll-hint"] {
                            "Open this page in multiple tabs — votes appear instantly across all sessions."
                        }
                    }

                    // ── question ──────────────────────────────────────────────
                    div[class="poll-question"] {
                        div[class="question-label"] { "Current question" }
                        div[class="question-text", text="question"] { "…" }
                    }

                    // ── options (open state) ──────────────────────────────────
                    div[class="poll-options", show="open"] {
                        div[each="options"] {
                            div[class="option-row", style="display:none"] {
                                div[class="option-header"] {
                                    span[class="option-label", field="label"] {}
                                    span[class="option-votes"] {
                                        span[field="votes"] { "0" }
                                        " votes"
                                    }
                                }
                                div[class="vote-bar-track"] {
                                    div[class="vote-bar-fill"] {
                                        span[class="vote-pct", field="percent_str"] { "0%" }
                                    }
                                }
                                button[class="btn btn-vote", on="click->vote"] {
                                    "Vote"
                                }
                            }
                        }
                    }

                    // ── results (closed state) ────────────────────────────────
                    div[class="poll-results", show="closed"] {
                        div[class="winner-banner"] {
                            div[class="winner-label"] { "Winner" }
                            div[class="winner-name", text="winner"] { "—" }
                            div[class="winner-votes"] {
                                span[text="winner_votes"] { "0" }
                                " votes"
                            }
                        }
                        div[each="options"] {
                            div[class="result-row", style="display:none"] {
                                div[class="result-header"] {
                                    span[class="result-rank", field="rank"] { "" }
                                    span[class="result-label", field="label"] {}
                                    span[class="result-pct", field="percent_str"] { "0%" }
                                }
                                div[class="result-bar-track"] {
                                    div[class="result-bar-fill"] {
                                        span[class="result-votes", field="votes"] { "0" }
                                    }
                                }
                            }
                        }
                    }

                    // ── footer ────────────────────────────────────────────────
                    div[class="poll-footer"] {
                        div[class="total-count"] {
                            span[text="total_votes"] { "0" }
                            " total votes"
                        }
                        div[class="poll-actions"] {
                            button[class="btn btn-close", show="open",
                                   on="click->close_poll"] {
                                "Close Poll"
                            }
                            button[class="btn btn-reset", show="closed",
                                   on="click->reset"] {
                                "Reset & Reopen"
                            }
                        }
                    }

                }
            }

        }))
        .build();

    let mut machines = HashMap::new();
    machines.insert("poll".to_string(), machine);

    let pkg_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg");
    let app = router_with(machines, store, pubsub)
        .nest_service("/pkg", ServeDir::new(pkg_dir));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3008").await.unwrap();
    println!("Foster collab → http://localhost:3008");
    println!("Open multiple tabs to see real-time votes via Redis pub/sub");
    axum::serve(listener, app).await.unwrap();
}
