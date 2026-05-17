use foster_core::{html, page, MachineBuilder, MachineError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tower_http::services::ServeDir;

// ── context ───────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
struct FormCtx {
    // Step 1 — personal
    first_name:       String,
    last_name:        String,
    job_title:        String,
    first_name_error: String,
    last_name_error:  String,
    step1_valid:      bool,

    // Step 2 — contact
    email:       String,
    phone:       String,
    company:     String,
    email_error: String,
    step2_valid: bool,

    // Step 3 — preferences
    track:       String,
    dietary:     String,
    tshirt_size: String,
    newsletter:  String,
    track_error: String,
    step3_valid: bool,

    // Submission
    confirmation_code: String,
    submit_error:      String,

    // Progress (0–100, displayed as CSS width via progress_style)
    progress:       u32,
    progress_style: String,
}

impl Default for FormCtx {
    fn default() -> Self {
        Self {
            first_name:       String::new(),
            last_name:        String::new(),
            job_title:        String::new(),
            first_name_error: String::new(),
            last_name_error:  String::new(),
            step1_valid:      false,
            email:            String::new(),
            phone:            String::new(),
            company:          String::new(),
            email_error:      String::new(),
            step2_valid:      false,
            track:            "engineering".to_string(),
            dietary:          "none".to_string(),
            tshirt_size:      "M".to_string(),
            newsletter:       "yes".to_string(),
            track_error:      String::new(),
            step3_valid:      false,
            confirmation_code: String::new(),
            submit_error:     String::new(),
            progress:         0,
            progress_style:   "width:0%".to_string(),
        }
    }
}

fn set_progress(ctx: &mut FormCtx, pct: u32) {
    ctx.progress       = pct;
    ctx.progress_style = format!("width:{}%", pct);
}

// ── reducers ──────────────────────────────────────────────────────────────────

fn validate1(mut ctx: FormCtx, payload: Value) -> Result<FormCtx, MachineError> {
    let first = payload["first_name"].as_str().unwrap_or("").trim().to_string();
    let last  = payload["last_name"].as_str().unwrap_or("").trim().to_string();
    let job   = payload["job_title"].as_str().unwrap_or("").trim().to_string();

    ctx.first_name       = first.clone();
    ctx.last_name        = last.clone();
    ctx.job_title        = job;
    ctx.first_name_error = if first.is_empty() { "First name is required".to_string() } else { String::new() };
    ctx.last_name_error  = if last.is_empty()  { "Last name is required".to_string()  } else { String::new() };
    ctx.step1_valid      = ctx.first_name_error.is_empty() && ctx.last_name_error.is_empty();
    let pct = if ctx.step1_valid { 20 } else { 0 };
    set_progress(&mut ctx, pct);
    Ok(ctx)
}

fn advance1(ctx: FormCtx, _payload: Value) -> Result<FormCtx, MachineError> {
    if !ctx.step1_valid {
        return Err(MachineError::ReducerError("complete step 1 first".into()));
    }
    Ok(ctx)
}

fn validate2(mut ctx: FormCtx, payload: Value) -> Result<FormCtx, MachineError> {
    let email   = payload["email"].as_str().unwrap_or("").trim().to_string();
    let phone   = payload["phone"].as_str().unwrap_or("").trim().to_string();
    let company = payload["company"].as_str().unwrap_or("").trim().to_string();

    ctx.email       = email.clone();
    ctx.phone       = phone;
    ctx.company     = company;
    ctx.email_error = if email.is_empty() {
        "Email is required".to_string()
    } else if !email.contains('@') {
        "Enter a valid email address".to_string()
    } else {
        String::new()
    };
    ctx.step2_valid = ctx.email_error.is_empty();
    let pct = if ctx.step2_valid { 53 } else { 20 };
    set_progress(&mut ctx, pct);
    Ok(ctx)
}

fn advance2(ctx: FormCtx, _payload: Value) -> Result<FormCtx, MachineError> {
    if !ctx.step2_valid {
        return Err(MachineError::ReducerError("complete step 2 first".into()));
    }
    Ok(ctx)
}

fn validate3(mut ctx: FormCtx, payload: Value) -> Result<FormCtx, MachineError> {
    let track    = payload["track"].as_str().unwrap_or("").trim().to_string();
    let dietary  = payload["dietary"].as_str().unwrap_or("none").to_string();
    let tshirt   = payload["tshirt_size"].as_str().unwrap_or("M").to_string();
    let newslttr = payload["newsletter"].as_str().unwrap_or("yes").to_string();

    ctx.track       = if track.is_empty() { ctx.track.clone() } else { track.clone() };
    ctx.dietary     = if dietary.is_empty() { ctx.dietary.clone() } else { dietary };
    ctx.tshirt_size = if tshirt.is_empty() { ctx.tshirt_size.clone() } else { tshirt };
    ctx.newsletter  = if newslttr.is_empty() { ctx.newsletter.clone() } else { newslttr };
    ctx.track_error = if ctx.track.is_empty() { "Please select a track".to_string() } else { String::new() };
    ctx.step3_valid = ctx.track_error.is_empty();
    let pct = if ctx.step3_valid { 80 } else { 53 };
    set_progress(&mut ctx, pct);
    Ok(ctx)
}

fn advance3(ctx: FormCtx, _payload: Value) -> Result<FormCtx, MachineError> {
    if !ctx.step3_valid {
        return Err(MachineError::ReducerError("complete step 3 first".into()));
    }
    Ok(ctx)
}

fn back1(mut ctx: FormCtx, _: Value) -> Result<FormCtx, MachineError> {
    ctx.step1_valid = false;
    set_progress(&mut ctx, 0);
    Ok(ctx)
}

fn back2(mut ctx: FormCtx, _: Value) -> Result<FormCtx, MachineError> {
    ctx.step2_valid = false;
    set_progress(&mut ctx, 20);
    Ok(ctx)
}

fn back3(mut ctx: FormCtx, _: Value) -> Result<FormCtx, MachineError> {
    ctx.step3_valid = false;
    set_progress(&mut ctx, 53);
    Ok(ctx)
}

fn submit(mut ctx: FormCtx, _: Value) -> Result<FormCtx, MachineError> {
    ctx.confirmation_code = "CONF-2026-FOSTER".to_string();
    ctx.submit_error      = String::new();
    set_progress(&mut ctx, 100);
    Ok(ctx)
}

fn reset(_ctx: FormCtx, _: Value) -> Result<FormCtx, MachineError> {
    Ok(FormCtx::default())
}

// ── main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let ctx      = FormCtx::default();
    let ctx_json = serde_json::to_value(&ctx).unwrap();

    let machine = MachineBuilder::new("form", "step1", ctx_json)
        .state("step2")
        .state("step3")
        .state("review")
        .state("done")
        .typed_on("step1",  "validate1", "step1",  validate1)
        .typed_on("step1",  "advance1",  "step2",  advance1)
        .typed_on("step2",  "validate2", "step2",  validate2)
        .typed_on("step2",  "advance2",  "step3",  advance2)
        .typed_on("step2",  "back1",     "step1",  back1)
        .typed_on("step3",  "validate3", "step3",  validate3)
        .typed_on("step3",  "advance3",  "review", advance3)
        .typed_on("step3",  "back2",     "step2",  back2)
        .typed_on("review", "submit",    "done",   submit)
        .typed_on("review", "back3",     "step3",  back3)
        .typed_on("done",   "reset",     "step1",  reset)
        .template(page("Conference Registration", include_str!("../static/style.css"), html! {

            div[class="form-page"] {
                div[class="form-container"] {

                    // ── header ────────────────────────────────────────────────
                    div[class="form-header"] {
                        div[class="form-logo"] { "🌐" }
                        h1[class="form-title"] { "Foster Conference 2026" }
                        p[class="form-subtitle"] { "Annual Rust + WASM Developer Summit" }
                    }

                    // ── progress bar ──────────────────────────────────────────
                    div[class="progress-track"] {
                        div[class="progress-fill", bind_attr="style=ctx:progress_style"] {}
                    }
                    div[class="progress-steps"] {
                        span[class="step-dot", show="step1"]        { "● Personal" }
                        span[class="step-dot step-done", show="step2,step3,review,done"] { "✓ Personal" }
                        span[class="step-sep"] { "—" }
                        span[class="step-dot step-inactive", show="step1"]  { "○ Contact" }
                        span[class="step-dot", show="step2"]        { "● Contact" }
                        span[class="step-dot step-done", show="step3,review,done"] { "✓ Contact" }
                        span[class="step-sep"] { "—" }
                        span[class="step-dot step-inactive", show="step1,step2"] { "○ Preferences" }
                        span[class="step-dot", show="step3"]        { "● Preferences" }
                        span[class="step-dot step-done", show="review,done"] { "✓ Preferences" }
                    }

                    // ── step 1 ────────────────────────────────────────────────
                    div[class="form-panel", show="step1"] {
                        h2[class="panel-title"] { "Personal Information" }
                        div[class="field-group"] {
                            div[class="form-row half"] {
                                label[class="field-label"] { "First Name *" }
                                input[type="text", class="field-input",
                                      collect="first_name", value="first_name",
                                      placeholder="Jane"]
                                div[class="field-error", if="first_name_error",
                                    text="first_name_error"] {}
                            }
                            div[class="form-row half"] {
                                label[class="field-label"] { "Last Name *" }
                                input[type="text", class="field-input",
                                      collect="last_name", value="last_name",
                                      placeholder="Smith"]
                                div[class="field-error", if="last_name_error",
                                    text="last_name_error"] {}
                            }
                        }
                        div[class="form-row"] {
                            label[class="field-label"] { "Job Title" }
                            input[type="text", class="field-input",
                                  collect="job_title", value="job_title",
                                  placeholder="Software Engineer"]
                        }
                        div[class="panel-actions"] {
                            button[class="btn btn-primary",
                                   if=r#"{"field":"step1_valid","op":"eq","value":false}"#,
                                   on="click->validate1"] {
                                "Check & Continue →"
                            }
                            button[class="btn btn-primary",
                                   if="step1_valid",
                                   on="click->advance1"] {
                                "Continue →"
                            }
                        }
                    }

                    // ── step 2 ────────────────────────────────────────────────
                    div[class="form-panel", show="step2"] {
                        h2[class="panel-title"] { "Contact Details" }
                        div[class="form-row"] {
                            label[class="field-label"] { "Email Address *" }
                            input[type="email", class="field-input",
                                  collect="email", value="email",
                                  placeholder="jane@example.com"]
                            div[class="field-error", if="email_error",
                                text="email_error"] {}
                        }
                        div[class="field-group"] {
                            div[class="form-row half"] {
                                label[class="field-label"] { "Phone" }
                                input[type="tel", class="field-input",
                                      collect="phone", value="phone",
                                      placeholder="+1 555 000 0000"]
                            }
                            div[class="form-row half"] {
                                label[class="field-label"] { "Company" }
                                input[type="text", class="field-input",
                                      collect="company", value="company",
                                      placeholder="Acme Corp"]
                            }
                        }
                        div[class="panel-actions"] {
                            button[class="btn btn-ghost", on="click->back1"] { "← Back" }
                            button[class="btn btn-primary",
                                   if=r#"{"field":"step2_valid","op":"eq","value":false}"#,
                                   on="click->validate2"] {
                                "Check & Continue →"
                            }
                            button[class="btn btn-primary",
                                   if="step2_valid",
                                   on="click->advance2"] {
                                "Continue →"
                            }
                        }
                    }

                    // ── step 3 ────────────────────────────────────────────────
                    div[class="form-panel", show="step3"] {
                        h2[class="panel-title"] { "Conference Preferences" }
                        div[class="field-group"] {
                            div[class="form-row half"] {
                                label[class="field-label"] { "Track *" }
                                select[class="field-select",
                                       collect="track", value="track"] {
                                    option[value="engineering"] { "Engineering" }
                                    option[value="product"]     { "Product" }
                                    option[value="design"]      { "Design" }
                                    option[value="leadership"]  { "Leadership" }
                                }
                                div[class="field-error", if="track_error",
                                    text="track_error"] {}
                            }
                            div[class="form-row half"] {
                                label[class="field-label"] { "Dietary" }
                                select[class="field-select",
                                       collect="dietary", value="dietary"] {
                                    option[value="none"]         { "No restrictions" }
                                    option[value="vegetarian"]   { "Vegetarian" }
                                    option[value="vegan"]        { "Vegan" }
                                    option[value="gluten-free"]  { "Gluten-free" }
                                }
                            }
                        }
                        div[class="field-group"] {
                            div[class="form-row half"] {
                                label[class="field-label"] { "T-shirt Size" }
                                select[class="field-select",
                                       collect="tshirt_size", value="tshirt_size"] {
                                    option[value="XS"] { "XS" }
                                    option[value="S"]  { "S" }
                                    option[value="M"]  { "M (default)" }
                                    option[value="L"]  { "L" }
                                    option[value="XL"] { "XL" }
                                    option[value="2XL"] { "2XL" }
                                }
                            }
                            div[class="form-row half"] {
                                label[class="field-label"] { "Newsletter" }
                                select[class="field-select",
                                       collect="newsletter", value="newsletter"] {
                                    option[value="yes"] { "Yes, subscribe me" }
                                    option[value="no"]  { "No thanks" }
                                }
                            }
                        }
                        div[class="panel-actions"] {
                            button[class="btn btn-ghost", on="click->back2"] { "← Back" }
                            button[class="btn btn-primary",
                                   if=r#"{"field":"step3_valid","op":"eq","value":false}"#,
                                   on="click->validate3"] {
                                "Check & Review →"
                            }
                            button[class="btn btn-primary",
                                   if="step3_valid",
                                   on="click->advance3"] {
                                "Review →"
                            }
                        }
                    }

                    // ── review ────────────────────────────────────────────────
                    div[class="form-panel", show="review"] {
                        h2[class="panel-title"] { "Review Your Registration" }
                        div[class="review-grid"] {
                            div[class="review-section"] {
                                div[class="review-section-title"] { "Personal" }
                                div[class="review-row"] {
                                    span[class="review-key"] { "Name" }
                                    span[class="review-val"] {
                                        span[text="first_name"] { "" }
                                        " "
                                        span[text="last_name"] { "" }
                                    }
                                }
                                div[class="review-row", if="job_title"] {
                                    span[class="review-key"] { "Title" }
                                    span[class="review-val", text="job_title"] { "" }
                                }
                            }
                            div[class="review-section"] {
                                div[class="review-section-title"] { "Contact" }
                                div[class="review-row"] {
                                    span[class="review-key"] { "Email" }
                                    span[class="review-val", text="email"] { "" }
                                }
                                div[class="review-row", if="phone"] {
                                    span[class="review-key"] { "Phone" }
                                    span[class="review-val", text="phone"] { "" }
                                }
                                div[class="review-row", if="company"] {
                                    span[class="review-key"] { "Company" }
                                    span[class="review-val", text="company"] { "" }
                                }
                            }
                            div[class="review-section"] {
                                div[class="review-section-title"] { "Preferences" }
                                div[class="review-row"] {
                                    span[class="review-key"] { "Track" }
                                    span[class="review-val", text="track"] { "" }
                                }
                                div[class="review-row"] {
                                    span[class="review-key"] { "Dietary" }
                                    span[class="review-val", text="dietary"] { "" }
                                }
                                div[class="review-row"] {
                                    span[class="review-key"] { "T-shirt" }
                                    span[class="review-val", text="tshirt_size"] { "" }
                                }
                                div[class="review-row"] {
                                    span[class="review-key"] { "Newsletter" }
                                    span[class="review-val", text="newsletter"] { "" }
                                }
                            }
                        }
                        div[class="review-note"] {
                            "By registering, you agree to the conference terms and code of conduct."
                        }
                        div[class="panel-actions"] {
                            button[class="btn btn-ghost", on="click->back3"] { "← Edit" }
                            button[class="btn btn-primary btn-lg",
                                   on="click->submit",
                                   optimistic="done"] {
                                "Complete Registration"
                            }
                        }
                    }

                    // ── done ──────────────────────────────────────────────────
                    div[class="form-panel form-panel-success", show="done"] {
                        div[class="success-icon"] { "✅" }
                        h2[class="panel-title"] { "You're registered!" }
                        p[class="success-msg"] {
                            "Welcome to Foster Conference 2026. See you in San Francisco!"
                        }
                        div[class="confirmation-box"] {
                            div[class="confirmation-label"] { "Confirmation Code" }
                            div[class="confirmation-code", text="confirmation_code"] { "—" }
                        }
                        p[class="success-hint"] {
                            "A confirmation email has been sent to "
                            span[text="email"] { "" }
                            "."
                        }
                        div[class="panel-actions"] {
                            button[class="btn btn-outline", on="click->reset"] {
                                "Register another attendee"
                            }
                        }
                    }

                }
            }

        }))
        .build();

    let mut machines = HashMap::new();
    machines.insert("form".to_string(), machine);

    let pkg_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg");
    let app = foster_server::router(machines)
        .nest_service("/pkg", ServeDir::new(pkg_dir));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3007").await.unwrap();
    println!("Foster form → http://localhost:3007");
    axum::serve(listener, app).await.unwrap();
}
