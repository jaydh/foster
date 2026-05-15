use foster_core::{html, page, MachineBuilder, MachineError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tower_http::services::ServeDir;

// ── data model ────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Default)]
struct Block {
    id:         String,
    block_type:  String,    // h1 h2 h3 p bullet numbered quote code callout todo
    content:    String,
    checked:    bool,
    number:     u32,
    type_label: String,     // "H1", "•", "☐", "1." — display prefix
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct NotionCtx {
    doc_title:   String,
    blocks:      Vec<Block>,

    // flattened active block (for edit panel labels)
    active_id:      String,
    active_type:    String,
    active_content: String,

    // draft (while in editing state)
    draft_content: String,
    draft_type:   String,

    // title draft
    draft_title: String,

    // id of block just created by add_block (remove on discard)
    new_block_id: String,

    // stats
    block_count: u32,
    word_count:  u32,
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn label_for(block_type: &str, checked: bool, number: u32) -> String {
    match block_type {
        "h1"       => "H1".to_string(),
        "h2"       => "H2".to_string(),
        "h3"       => "H3".to_string(),
        "p"        => "¶".to_string(),
        "bullet"   => "•".to_string(),
        "numbered" => format!("{}.", number),
        "quote"    => "❝".to_string(),
        "code"     => "</>".to_string(),
        "callout"  => "💡".to_string(),
        "todo"     => if checked { "☑".to_string() } else { "☐".to_string() },
        _          => "•".to_string(),
    }
}

fn make_block(id: &str, block_type: &str, content: &str, checked: bool) -> Block {
    Block {
        id:         id.to_string(),
        block_type:  block_type.to_string(),
        content:    content.to_string(),
        checked,
        number:     0,
        type_label: label_for(block_type, checked, 0),
    }
}

fn renumber(blocks: &mut Vec<Block>) {
    let mut n = 0u32;
    for b in blocks.iter_mut() {
        if b.block_type == "numbered" {
            n += 1;
            b.number = n;
            b.type_label = format!("{}.", n);
        } else {
            if b.block_type != "numbered" { n = 0; }
            b.type_label = label_for(&b.block_type, b.checked, b.number);
        }
    }
}

fn recompute_stats(ctx: &mut NotionCtx) {
    ctx.block_count = ctx.blocks.len() as u32;
    ctx.word_count  = ctx.blocks.iter()
        .map(|b| b.content.split_whitespace().count() as u32)
        .sum::<u32>()
        + ctx.doc_title.split_whitespace().count() as u32;
}

fn set_active(ctx: &mut NotionCtx, id: &str) {
    if let Some(b) = ctx.blocks.iter().find(|b| b.id == id) {
        ctx.active_id      = b.id.clone();
        ctx.active_type    = b.block_type.clone();
        ctx.active_content = b.content.clone();
        ctx.draft_content  = b.content.clone();
        ctx.draft_type     = b.block_type.clone();
    }
}

fn next_id(ctx: &NotionCtx) -> String {
    let max = ctx.blocks.iter()
        .filter_map(|b| b.id.strip_prefix("b-").and_then(|n| n.parse::<u32>().ok()))
        .max()
        .unwrap_or(0);
    format!("b-{}", max + 1)
}

// ── reducers ──────────────────────────────────────────────────────────────────

fn focus_block(mut ctx: NotionCtx, payload: Value) -> Result<NotionCtx, MachineError> {
    let id = payload["id"].as_str().unwrap_or("").to_string();
    set_active(&mut ctx, &id);
    ctx.new_block_id = String::new();
    Ok(ctx)
}

fn add_block(mut ctx: NotionCtx, payload: Value) -> Result<NotionCtx, MachineError> {
    let after_id   = payload["id"].as_str().unwrap_or("").to_string();
    let block_type = payload["block_type"].as_str().unwrap_or("p").to_string();
    let new_id     = next_id(&ctx);
    let new_block  = make_block(&new_id, &block_type, "", false);

    match ctx.blocks.iter().position(|b| b.id == after_id) {
        Some(i) => ctx.blocks.insert(i + 1, new_block),
        None    => ctx.blocks.push(new_block),
    }

    ctx.active_id      = new_id.clone();
    ctx.active_type    = block_type.clone();
    ctx.active_content = String::new();
    ctx.draft_content  = String::new();
    ctx.draft_type     = block_type;
    ctx.new_block_id   = new_id;

    renumber(&mut ctx.blocks);
    recompute_stats(&mut ctx);
    Ok(ctx)
}

fn commit_edit(mut ctx: NotionCtx, payload: Value) -> Result<NotionCtx, MachineError> {
    let content    = payload["draft_content"].as_str().unwrap_or("").to_string();
    let block_type = payload["draft_type"].as_str().unwrap_or("p").to_string();

    if let Some(block) = ctx.blocks.iter_mut().find(|b| b.id == ctx.active_id) {
        block.content   = content;
        block.block_type = block_type;
    }

    renumber(&mut ctx.blocks);
    recompute_stats(&mut ctx);
    ctx.new_block_id  = String::new();
    ctx.draft_content = String::new();
    Ok(ctx)
}

fn discard_edit(mut ctx: NotionCtx, _payload: Value) -> Result<NotionCtx, MachineError> {
    if !ctx.new_block_id.is_empty() {
        ctx.blocks.retain(|b| b.id != ctx.new_block_id);
        renumber(&mut ctx.blocks);
        recompute_stats(&mut ctx);
    }
    ctx.new_block_id  = String::new();
    ctx.draft_content = String::new();
    Ok(ctx)
}

fn change_type(mut ctx: NotionCtx, payload: Value) -> Result<NotionCtx, MachineError> {
    ctx.draft_type    = payload["draft_type"].as_str().unwrap_or("p").to_string();
    ctx.draft_content = payload["draft_content"].as_str().unwrap_or("").to_string();
    Ok(ctx)
}

fn delete_block(mut ctx: NotionCtx, payload: Value) -> Result<NotionCtx, MachineError> {
    let id = payload["id"].as_str().unwrap_or("").to_string();
    ctx.blocks.retain(|b| b.id != id);
    renumber(&mut ctx.blocks);
    recompute_stats(&mut ctx);
    Ok(ctx)
}

fn move_up(mut ctx: NotionCtx, payload: Value) -> Result<NotionCtx, MachineError> {
    let id = payload["id"].as_str().unwrap_or("").to_string();
    if let Some(i) = ctx.blocks.iter().position(|b| b.id == id) {
        if i > 0 {
            ctx.blocks.swap(i, i - 1);
            renumber(&mut ctx.blocks);
        }
    }
    Ok(ctx)
}

fn move_down(mut ctx: NotionCtx, payload: Value) -> Result<NotionCtx, MachineError> {
    let id = payload["id"].as_str().unwrap_or("").to_string();
    if let Some(i) = ctx.blocks.iter().position(|b| b.id == id) {
        if i + 1 < ctx.blocks.len() {
            ctx.blocks.swap(i, i + 1);
            renumber(&mut ctx.blocks);
        }
    }
    Ok(ctx)
}

fn toggle_todo(mut ctx: NotionCtx, payload: Value) -> Result<NotionCtx, MachineError> {
    let id = payload["id"].as_str().unwrap_or("").to_string();
    if let Some(b) = ctx.blocks.iter_mut().find(|b| b.id == id && b.block_type == "todo") {
        b.checked    = !b.checked;
        b.type_label = label_for("todo", b.checked, 0);
    }
    Ok(ctx)
}

fn update_title(mut ctx: NotionCtx, payload: Value) -> Result<NotionCtx, MachineError> {
    let title = payload["draft_title"].as_str().unwrap_or("").trim().to_string();
    if !title.is_empty() {
        ctx.doc_title   = title.clone();
        ctx.draft_title = title;
    }
    recompute_stats(&mut ctx);
    Ok(ctx)
}

// ── seed document ─────────────────────────────────────────────────────────────

fn seed_ctx() -> NotionCtx {
    let mut blocks = vec![
        make_block("b-1",  "h1",       "Foster Framework",                                                        false),
        make_block("b-2",  "p",        "An open-source state machine framework for reactive web UIs in Rust + WASM.", false),
        make_block("b-3",  "h2",       "Key Features",                                                            false),
        make_block("b-4",  "bullet",   "Rust-native state machines with type-safe transitions",                    false),
        make_block("b-5",  "bullet",   "Zero-config WASM client — no JavaScript required",                        false),
        make_block("b-6",  "bullet",   "Differential SSE rendering for minimal payload",                          false),
        make_block("b-7",  "bullet",   "Playwright test generation from machine definition",                       false),
        make_block("b-8",  "h2",       "Quick Start",                                                             false),
        make_block("b-9",  "numbered", "Install wasm-pack: cargo install wasm-pack",                              false),
        make_block("b-10", "numbered", "Build WASM client: ./scripts/build-wasm.sh",                              false),
        make_block("b-11", "numbered", "Start a demo: ./scripts/demo.sh counter",                                 false),
        make_block("b-12", "numbered", "Open http://localhost:3000",                                              false),
        make_block("b-13", "callout",  "The server owns all state. The client is a pure render layer.",           false),
        make_block("b-14", "quote",    "Write the machine definition once — tests, types, and templates derive.", false),
        make_block("b-15", "h2",       "Architecture",                                                            false),
        make_block("b-16", "code",     "MachineBuilder::new(\"app\", \"idle\", json!({}))\n  .state(\"error\")\n  .on(\"idle\", \"fail\", \"error\", reducer)\n  .build()", false),
        make_block("b-17", "h2",       "Getting Started Checklist",                                               false),
        make_block("b-18", "todo",     "Read CLAUDE.md",                                                          true),
        make_block("b-19", "todo",     "Build the WASM client with wasm-pack",                                    true),
        make_block("b-20", "todo",     "Add a new transition to an existing example",                             false),
        make_block("b-21", "todo",     "Build a custom machine from scratch",                                     false),
    ];
    renumber(&mut blocks);

    let mut ctx = NotionCtx {
        doc_title:   "Foster Framework".to_string(),
        draft_title: "Foster Framework".to_string(),
        blocks,
        ..Default::default()
    };
    recompute_stats(&mut ctx);
    ctx
}

// ── main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let ctx      = seed_ctx();
    let ctx_json = serde_json::to_value(&ctx).unwrap();

    let machine = MachineBuilder::new("notion", "reading", ctx_json)
        .state("editing")
        .typed_on("reading", "focus_block",  "editing", focus_block)
        .typed_on("reading", "add_block",    "editing", add_block)
        .typed_on("reading", "delete_block", "reading", delete_block)
        .typed_on("reading", "move_up",      "reading", move_up)
        .typed_on("reading", "move_down",    "reading", move_down)
        .typed_on("reading", "toggle_todo",  "reading", toggle_todo)
        .typed_on("reading", "update_title", "reading", update_title)
        .typed_on("editing", "commit_edit",  "reading", commit_edit)
        .typed_on("editing", "discard_edit", "reading", discard_edit)
        .typed_on("editing", "change_type",  "editing", change_type)
        .template(page("Notion — Block Editor", include_str!("../static/style.css"), html! {

            div[class="notion-app"] {

                // ── sidebar ──────────────────────────────────────────────────
                div[class="notion-sidebar"] {
                    div[class="sidebar-workspace"] {
                        div[class="workspace-icon"] { "N" }
                        div[class="workspace-info"] {
                            div[class="workspace-name"] { "My Workspace" }
                            div[class="workspace-plan"] { "Free plan" }
                        }
                    }
                    div[class="sidebar-divider"]
                    div[class="sidebar-section"] {
                        div[class="sidebar-section-label"] { "Pages" }
                        a[class="page-item page-item-active"] {
                            span[class="page-icon"] { "📄" }
                            span { "Foster Framework" }
                        }
                        a[class="page-item"] {
                            span[class="page-icon"] { "📝" }
                            span { "Meeting Notes" }
                        }
                        a[class="page-item"] {
                            span[class="page-icon"] { "📊" }
                            span { "Q3 Roadmap" }
                        }
                        a[class="page-item"] {
                            span[class="page-icon"] { "✅" }
                            span { "Sprint Board" }
                        }
                        a[class="page-item"] {
                            span[class="page-icon"] { "🔖" }
                            span { "Release Notes" }
                        }
                    }
                    div[class="sidebar-footer"] {
                        div[class="sidebar-stat"] {
                            span[text="block_count"] { "0" }
                            " blocks · "
                            span[text="word_count"] { "0" }
                            " words"
                        }
                    }
                }

                // ── main ─────────────────────────────────────────────────────
                div[class="notion-main"] {

                    // topbar
                    div[class="notion-topbar"] {
                        div[class="topbar-breadcrumb"] {
                            span[class="breadcrumb-dim"] { "My Workspace  /" }
                            span[class="breadcrumb-cur", text="doc_title"] { "Untitled" }
                        }
                        div[class="topbar-actions"] {
                            button[class="btn btn-ghost btn-sm", show="editing",
                                   on="click->discard_edit"] { "Cancel editing" }
                        }
                    }

                    // doc content area
                    div[class="notion-doc"] {

                        // doc icon + title
                        div[class="doc-cover"] {
                            div[class="doc-emoji"] { "📄" }
                        }
                        div[class="doc-title-row"] {
                            input[type="text", class="doc-title-input",
                                  collect="draft_title",
                                  value="doc_title",
                                  on="change->update_title",
                                  placeholder="Untitled"]
                        }

                        // ── edit panel (editing state) ────────────────────────
                        div[class="edit-panel", show="editing"] {
                            div[class="edit-panel-header"] {
                                div[class="edit-panel-label"] {
                                    "Editing "
                                    span[class="edit-type-tag", text="active_type"] { "p" }
                                    " block"
                                }
                                div[class="edit-panel-controls"] {
                                    select[class="type-select",
                                           value="draft_type",
                                           on="change->change_type",
                                           collect="draft_type"] {
                                        option[value="h1"]       { "Heading 1" }
                                        option[value="h2"]       { "Heading 2" }
                                        option[value="h3"]       { "Heading 3" }
                                        option[value="p"]        { "Paragraph" }
                                        option[value="bullet"]   { "Bullet list" }
                                        option[value="numbered"] { "Numbered list" }
                                        option[value="quote"]    { "Quote" }
                                        option[value="code"]     { "Code" }
                                        option[value="callout"]  { "Callout" }
                                        option[value="todo"]     { "To-do" }
                                    }
                                    button[class="btn btn-ghost", on="click->discard_edit"] { "Discard" }
                                }
                            }
                            textarea[class="block-editor",
                                     value="draft_content",
                                     collect="draft_content",
                                     placeholder="Start typing…"]
                            div[class="edit-panel-footer"] {
                                button[class="btn btn-primary", on="click->commit_edit"] {
                                    "Save changes"
                                }
                            }
                        }

                        // ── block list ────────────────────────────────────────
                        div[class="blocks-area"] {
                            div[each="blocks"] {
                                div[class="block-row", style="display:none"] {
                                    button[class="block-type-btn",
                                           on="click->toggle_todo",
                                           title="Toggle (todo blocks only)"] {
                                        span[class="block-badge", field="type_label"] {}
                                    }
                                    div[class="block-body"] {
                                        span[class="block-text", field="content"] {}
                                    }
                                    div[class="block-controls", show="reading"] {
                                        button[class="ctrl-btn",
                                               on="click->focus_block",
                                               title="Edit block"] { "✎" }
                                        button[class="ctrl-btn",
                                               on="click->add_block",
                                               payload=r#"{"block_type":"p"}"#,
                                               title="Add block below"] { "+" }
                                        button[class="ctrl-btn",
                                               on="click->move_up",
                                               title="Move up"] { "↑" }
                                        button[class="ctrl-btn",
                                               on="click->move_down",
                                               title="Move down"] { "↓" }
                                        button[class="ctrl-btn ctrl-btn-danger",
                                               on="click->delete_block",
                                               title="Delete block"] { "×" }
                                    }
                                }
                            }
                            button[class="add-block-btn",
                                   show="reading",
                                   on="click->add_block",
                                   payload=r#"{"id":"","block_type":"p"}"#] {
                                "＋  Add block"
                            }
                        }
                    }
                }
            }

        }))
        .build();

    let mut machines = HashMap::new();
    machines.insert("notion".to_string(), machine);

    let pkg_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg");
    let app = foster_server::router(machines)
        .nest_service("/pkg", ServeDir::new(pkg_dir));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3006").await.unwrap();
    println!("Foster notion → http://localhost:3006");
    axum::serve(listener, app).await.unwrap();
}
