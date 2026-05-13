use foster_core::{html, page, MachineBuilder, MachineError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};
use tower_http::services::ServeDir;

// ── typed context ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Item {
    name: String,
    qty: u32,
    price: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CheckoutCtx {
    items: Vec<Item>,
    total: u32,
    name: String,
    email: String,
    address: String,
    card_last4: String,
    order_id: String,
    error: String,
}

// ── reducers ──────────────────────────────────────────────────────────────────

fn save_shipping(mut ctx: CheckoutCtx, payload: Value) -> Result<CheckoutCtx, MachineError> {
    if let Some(v) = payload["name"].as_str()    { ctx.name    = v.trim().to_string(); }
    if let Some(v) = payload["email"].as_str()   { ctx.email   = v.trim().to_string(); }
    if let Some(v) = payload["address"].as_str() { ctx.address = v.trim().to_string(); }
    Ok(ctx)
}

fn save_payment(mut ctx: CheckoutCtx, payload: Value) -> Result<CheckoutCtx, MachineError> {
    let raw = payload["card"].as_str().unwrap_or("").replace([' ', '-'], "");
    ctx.card_last4 = if raw.len() >= 4 { raw[raw.len() - 4..].to_string() } else { raw };
    Ok(ctx)
}

fn succeed(mut ctx: CheckoutCtx, _: Value) -> Result<CheckoutCtx, MachineError> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    ctx.order_id = format!("ORD-{:06}", ms % 1_000_000);
    ctx.error    = String::new();
    Ok(ctx)
}

fn fail(mut ctx: CheckoutCtx, _: Value) -> Result<CheckoutCtx, MachineError> {
    ctx.error = "Your card was declined. Please check your details and try again.".to_string();
    Ok(ctx)
}

fn retry(mut ctx: CheckoutCtx, _: Value) -> Result<CheckoutCtx, MachineError> {
    ctx.error      = String::new();
    ctx.card_last4 = String::new();
    Ok(ctx)
}

fn new_order(mut ctx: CheckoutCtx, _: Value) -> Result<CheckoutCtx, MachineError> {
    ctx.name       = String::new();
    ctx.email      = String::new();
    ctx.address    = String::new();
    ctx.card_last4 = String::new();
    ctx.order_id   = String::new();
    ctx.error      = String::new();
    Ok(ctx)
}

// ── machine + server ──────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let machine = MachineBuilder::new("checkout", "cart", json!({
        "items": [
            { "name": "Mechanical Keyboard", "qty": 1, "price": 129 },
            { "name": "USB-C Hub",           "qty": 2, "price": 25  },
            { "name": "Desk Mat",            "qty": 1, "price": 35  }
        ],
        "total": 214,
        "name": "", "email": "", "address": "",
        "card_last4": "", "order_id": "", "error": ""
    }))
    .state("shipping")
    .state("payment")
    .state("review")
    .state("processing")
    .state("confirmed")
    .state("failed")
    .pass("cart",       "start_checkout", "shipping")
    .typed_on("shipping",   "save_shipping", "payment",    save_shipping)
    .pass("shipping",   "back",           "cart")
    .typed_on("payment",    "save_payment",  "review",     save_payment)
    .pass("payment",    "back",           "shipping")
    .pass("review",     "place_order",    "processing")
    .pass("review",     "back",           "payment")
    .typed_on("processing", "succeed",       "confirmed",  succeed)
    .typed_on("processing", "fail",          "failed",     fail)
    .typed_on("failed",     "retry",         "payment",    retry)
    .typed_on("confirmed",  "new_order",     "cart",       new_order)
    .template(page("Foster · Checkout", include_str!("../static/style.css"), html! {
        div[machine="checkout"] {
            header {
                span[class="wordmark"] { "Foster" }
                span[class="sep"] { "/" }
                span[class="page-title"] { "Checkout" }
                span[class="state-chip", state_label] { "…" }
            }

            // ── cart ────────────────────────────────────────────────────
            div[show="cart"] {
                div[class="section-title"] { "Your cart" }
                div[class="item-list", each="items"] {
                    div[class="item-row", style="display:none"] {
                        span[class="item-name", field="name"] { "—" }
                        span[class="item-meta"] {
                            span[class="item-qty", field="qty"] { "1" }
                            span[class="item-x"] { "×" }
                            span[class="item-price"] {
                                "$" span[field="price"] { "0" }
                            }
                        }
                    }
                }
                div[class="total-row"] {
                    span { "Total" }
                    span[class="total-amount"] { "$" span[text="total"] { "0" } }
                }
                div[class="actions"] {
                    button[class="btn-primary", on="click->start_checkout"] { "Checkout →" }
                }
            }

            // ── shipping ─────────────────────────────────────────────────
            div[show="shipping"] {
                div[class="steps"] {
                    span[class="step active"] { "1 Shipping" }
                    span[class="step"] { "2 Payment" }
                    span[class="step"] { "3 Review" }
                }
                div[class="section-title"] { "Shipping information" }
                div[class="form-group"] {
                    label { "Full name" }
                    input[type="text", placeholder="Jane Smith", collect="name", value="name"]
                }
                div[class="form-group"] {
                    label { "Email" }
                    input[type="email", placeholder="jane@example.com", collect="email", value="email"]
                }
                div[class="form-group"] {
                    label { "Address" }
                    input[type="text", placeholder="123 Main St, City, State", collect="address", value="address"]
                }
                div[class="actions"] {
                    button[class="btn-ghost", on="click->back"] { "← Cart" }
                    button[class="btn-primary", on="click->save_shipping"] { "Continue →" }
                }
            }

            // ── payment ──────────────────────────────────────────────────
            div[show="payment"] {
                div[class="steps"] {
                    span[class="step done"] { "✓ Shipping" }
                    span[class="step active"] { "2 Payment" }
                    span[class="step"] { "3 Review" }
                }
                div[class="section-title"] { "Payment" }
                div[class="form-group"] {
                    label { "Card number" }
                    input[type="text", placeholder="4242 4242 4242 4242", collect="card"]
                }
                div[class="card-hint"] { "Any digits work — this is a demo." }
                div[class="actions"] {
                    button[class="btn-ghost", on="click->back"] { "← Shipping" }
                    button[class="btn-primary", on="click->save_payment"] { "Review →" }
                }
            }

            // ── review ───────────────────────────────────────────────────
            div[show="review"] {
                div[class="steps"] {
                    span[class="step done"] { "✓ Shipping" }
                    span[class="step done"] { "✓ Payment" }
                    span[class="step active"] { "3 Review" }
                }
                div[class="section-title"] { "Order summary" }
                div[class="summary"] {
                    div[class="summary-row"] {
                        span[class="summary-key"] { "Name" }
                        span[text="name"] { "—" }
                    }
                    div[class="summary-row"] {
                        span[class="summary-key"] { "Email" }
                        span[text="email"] { "—" }
                    }
                    div[class="summary-row"] {
                        span[class="summary-key"] { "Address" }
                        span[text="address"] { "—" }
                    }
                    div[class="summary-row"] {
                        span[class="summary-key"] { "Card" }
                        span[class="card-mask"] { "···· " span[text="card_last4"] { "——" } }
                    }
                    div[class="summary-row summary-total"] {
                        span[class="summary-key"] { "Total" }
                        span[class="total-amount"] { "$" span[text="total"] { "0" } }
                    }
                }
                div[class="actions"] {
                    button[class="btn-ghost", on="click->back"] { "← Payment" }
                    button[class="btn-primary", on="click->place_order"] { "Place order →" }
                }
            }

            // ── processing ───────────────────────────────────────────────
            div[show="processing", class="centered"] {
                div[class="spinner"] {}
                p[class="processing-label"] { "Processing payment…" }
                p[class="demo-hint"] { "Demo — choose an outcome:" }
                div[class="actions"] {
                    button[class="btn-danger", on="click->fail"] { "✗ Decline" }
                    button[class="btn-safe",   on="click->succeed"] { "✓ Approve" }
                }
            }

            // ── confirmed ────────────────────────────────────────────────
            div[show="confirmed", class="centered outcome-ok"] {
                div[class="outcome-icon"] { "✓" }
                div[class="section-title"] { "Order confirmed" }
                div[class="order-id"] { span[text="order_id"] { "—" } }
                p[class="outcome-msg"] { "Your items will ship within 2 business days." }
                div[class="actions"] {
                    button[class="btn-ghost", on="click->new_order"] { "New order" }
                }
            }

            // ── failed ───────────────────────────────────────────────────
            div[show="failed", class="centered outcome-err"] {
                div[class="outcome-icon"] { "✗" }
                div[class="section-title"] { "Payment declined" }
                p[class="error-msg", text="error"] { "—" }
                div[class="actions"] {
                    button[class="btn-primary", on="click->retry"] { "Try again →" }
                }
            }
        }
    }))
    .build();

    let mut machines = HashMap::new();
    machines.insert("checkout".to_string(), machine);

    let pkg_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg");
    let app = foster_server::router(machines)
        .nest_service("/pkg", ServeDir::new(pkg_dir));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3004").await.unwrap();
    println!("Foster checkout → http://localhost:3004");
    axum::serve(listener, app).await.unwrap();
}
