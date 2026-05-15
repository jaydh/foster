use foster_core::{html, page, MachineBuilder, MachineError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tower_http::services::ServeDir;

// ── data model ────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Default)]
struct Comment {
    id:     String,
    author: String,
    body:   String,
    ts:     String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Issue {
    id:             String,
    seq:            u32,
    seq_str:        String,   // "EX-1"
    title:          String,
    description:    String,
    status:         String,   // backlog | todo | in_progress | done | cancelled
    status_label:   String,
    status_icon:    String,   // Unicode indicator
    priority:       String,   // urgent | high | medium | low | none
    priority_label: String,
    priority_icon:  String,
    assignee:       String,
    label:          String,
    due_date:       String,
    comments:       Vec<Comment>,
    comment_count:  u32,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct PlaneCtx {
    issues:          Vec<Issue>,
    filtered_issues: Vec<Issue>,

    // flattened active-issue fields (used by fx-text in the detail view)
    active_id:             String,
    active_seq_str:        String,
    active_title:          String,
    active_description:    String,
    active_status:         String,
    active_status_label:   String,
    active_status_icon:    String,
    active_priority:       String,
    active_priority_label: String,
    active_priority_icon:  String,
    active_assignee:       String,
    active_label:          String,
    active_due_date:       String,
    active_comments:       Vec<Comment>,

    // draft fields (create / edit forms + comment box)
    draft_title:       String,
    draft_description: String,
    draft_status:      String,
    draft_priority:    String,
    draft_assignee:    String,
    draft_label:       String,
    draft_due_date:    String,
    draft_comment:     String,

    // active filter values (shown in filter panel selects via fx-value)
    filter_status:   String,
    filter_priority: String,
    filter_assignee: String,

    // stats shown in the topbar
    open_count:  u32,
    done_count:  u32,
    total_count: u32,
    next_seq:    u32,
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn status_meta(s: &str) -> (&'static str, &'static str) {
    match s {
        "backlog"     => ("Backlog",     "○"),
        "todo"        => ("To Do",       "◎"),
        "in_progress" => ("In Progress", "◑"),
        "done"        => ("Done",        "●"),
        "cancelled"   => ("Cancelled",   "✕"),
        _             => ("Unknown",     "?"),
    }
}

fn priority_meta(p: &str) -> (&'static str, &'static str) {
    match p {
        "urgent" => ("Urgent", "!!"),
        "high"   => ("High",   "↑"),
        "medium" => ("Medium", "→"),
        "low"    => ("Low",    "↓"),
        _        => ("None",   "—"),
    }
}

fn build_issue(
    seq: u32, title: &str, description: &str,
    status: &str, priority: &str,
    assignee: &str, label: &str, due_date: &str,
    comments: Vec<Comment>,
) -> Issue {
    let (sl, si) = status_meta(status);
    let (pl, pi) = priority_meta(priority);
    let cc = comments.len() as u32;
    Issue {
        id: seq.to_string(), seq, seq_str: format!("EX-{seq}"),
        title: title.into(), description: description.into(),
        status: status.into(), status_label: sl.into(), status_icon: si.into(),
        priority: priority.into(), priority_label: pl.into(), priority_icon: pi.into(),
        assignee: assignee.into(), label: label.into(), due_date: due_date.into(),
        comment_count: cc, comments,
    }
}

fn c(id: u32, author: &str, body: &str, ts: &str) -> Comment {
    Comment { id: id.to_string(), author: author.into(), body: body.into(), ts: ts.into() }
}

fn recompute_filtered(ctx: &mut PlaneCtx) {
    ctx.filtered_issues = ctx.issues.iter()
        .filter(|i| {
            (ctx.filter_status.is_empty()   || i.status   == ctx.filter_status)   &&
            (ctx.filter_priority.is_empty() || i.priority == ctx.filter_priority) &&
            (ctx.filter_assignee.is_empty() || i.assignee.to_lowercase()
                .contains(&ctx.filter_assignee.to_lowercase()))
        })
        .cloned()
        .collect();
}

fn update_stats(ctx: &mut PlaneCtx) {
    ctx.total_count = ctx.issues.len() as u32;
    ctx.open_count  = ctx.issues.iter()
        .filter(|i| matches!(i.status.as_str(), "backlog"|"todo"|"in_progress"))
        .count() as u32;
    ctx.done_count  = ctx.issues.iter()
        .filter(|i| i.status == "done")
        .count() as u32;
}

fn set_active(ctx: &mut PlaneCtx, issue: &Issue) {
    ctx.active_id             = issue.id.clone();
    ctx.active_seq_str        = issue.seq_str.clone();
    ctx.active_title          = issue.title.clone();
    ctx.active_description    = issue.description.clone();
    ctx.active_status         = issue.status.clone();
    ctx.active_status_label   = issue.status_label.clone();
    ctx.active_status_icon    = issue.status_icon.clone();
    ctx.active_priority       = issue.priority.clone();
    ctx.active_priority_label = issue.priority_label.clone();
    ctx.active_priority_icon  = issue.priority_icon.clone();
    ctx.active_assignee       = issue.assignee.clone();
    ctx.active_label          = issue.label.clone();
    ctx.active_due_date       = issue.due_date.clone();
    ctx.active_comments       = issue.comments.clone();
}

fn next_cid(comments: &[Comment]) -> String {
    let max = comments.iter().filter_map(|c| c.id.parse::<u64>().ok()).max().unwrap_or(0);
    (max + 1).to_string()
}

// ── reducers ──────────────────────────────────────────────────────────────────

fn open_create(mut ctx: PlaneCtx, _: Value) -> Result<PlaneCtx, MachineError> {
    ctx.draft_title       = String::new();
    ctx.draft_description = String::new();
    ctx.draft_status      = "todo".into();
    ctx.draft_priority    = "medium".into();
    ctx.draft_assignee    = String::new();
    ctx.draft_label       = String::new();
    ctx.draft_due_date    = String::new();
    Ok(ctx)
}

fn save_create(mut ctx: PlaneCtx, p: Value) -> Result<PlaneCtx, MachineError> {
    let title  = p["draft_title"].as_str().unwrap_or("Untitled").trim().to_string();
    let desc   = p["draft_description"].as_str().unwrap_or("").trim().to_string();
    let status = p["draft_status"].as_str().unwrap_or("todo").to_string();
    let pri    = p["draft_priority"].as_str().unwrap_or("medium").to_string();
    let assign = p["draft_assignee"].as_str().unwrap_or("").trim().to_string();
    let label  = p["draft_label"].as_str().unwrap_or("").trim().to_string();
    let due    = p["draft_due_date"].as_str().unwrap_or("").trim().to_string();

    let seq = ctx.next_seq;
    ctx.next_seq += 1;
    ctx.issues.push(build_issue(seq, &title, &desc, &status, &pri, &assign, &label, &due, vec![]));
    update_stats(&mut ctx);
    recompute_filtered(&mut ctx);
    Ok(ctx)
}

fn cancel_create(ctx: PlaneCtx, _: Value) -> Result<PlaneCtx, MachineError> { Ok(ctx) }

fn open_issue(mut ctx: PlaneCtx, p: Value) -> Result<PlaneCtx, MachineError> {
    let id = p["id"].as_str().unwrap_or("").to_string();
    if let Some(issue) = ctx.issues.iter().find(|i| i.id == id).cloned() {
        set_active(&mut ctx, &issue);
        ctx.draft_comment = String::new();
    }
    Ok(ctx)
}

fn toggle_filter(ctx: PlaneCtx, _: Value) -> Result<PlaneCtx, MachineError> { Ok(ctx) }
fn close_filter(ctx: PlaneCtx, _: Value) -> Result<PlaneCtx, MachineError>  { Ok(ctx) }

fn apply_filter(mut ctx: PlaneCtx, p: Value) -> Result<PlaneCtx, MachineError> {
    ctx.filter_status   = p["filter_status"].as_str().unwrap_or("").to_string();
    ctx.filter_priority = p["filter_priority"].as_str().unwrap_or("").to_string();
    ctx.filter_assignee = p["filter_assignee"].as_str().unwrap_or("").trim().to_string();
    recompute_filtered(&mut ctx);
    Ok(ctx)
}

fn clear_filter(mut ctx: PlaneCtx, _: Value) -> Result<PlaneCtx, MachineError> {
    ctx.filter_status   = String::new();
    ctx.filter_priority = String::new();
    ctx.filter_assignee = String::new();
    recompute_filtered(&mut ctx);
    Ok(ctx)
}

fn back(ctx: PlaneCtx, _: Value) -> Result<PlaneCtx, MachineError> { Ok(ctx) }

fn start_edit(mut ctx: PlaneCtx, _: Value) -> Result<PlaneCtx, MachineError> {
    ctx.draft_title       = ctx.active_title.clone();
    ctx.draft_description = ctx.active_description.clone();
    ctx.draft_status      = ctx.active_status.clone();
    ctx.draft_priority    = ctx.active_priority.clone();
    ctx.draft_assignee    = ctx.active_assignee.clone();
    ctx.draft_label       = ctx.active_label.clone();
    ctx.draft_due_date    = ctx.active_due_date.clone();
    Ok(ctx)
}

fn save_edit(mut ctx: PlaneCtx, p: Value) -> Result<PlaneCtx, MachineError> {
    let title  = p["draft_title"].as_str().unwrap_or(&ctx.active_title).trim().to_string();
    let desc   = p["draft_description"].as_str().unwrap_or(&ctx.active_description).trim().to_string();
    let status = p["draft_status"].as_str().unwrap_or(&ctx.active_status).to_string();
    let pri    = p["draft_priority"].as_str().unwrap_or(&ctx.active_priority).to_string();
    let assign = p["draft_assignee"].as_str().unwrap_or("").trim().to_string();
    let label  = p["draft_label"].as_str().unwrap_or("").trim().to_string();
    let due    = p["draft_due_date"].as_str().unwrap_or("").trim().to_string();

    let id = ctx.active_id.clone();
    if let Some(issue) = ctx.issues.iter_mut().find(|i| i.id == id) {
        let (sl, si) = status_meta(&status);
        let (pl, pi) = priority_meta(&pri);
        issue.title          = title;
        issue.description    = desc;
        issue.status         = status;
        issue.status_label   = sl.into();
        issue.status_icon    = si.into();
        issue.priority       = pri;
        issue.priority_label = pl.into();
        issue.priority_icon  = pi.into();
        issue.assignee       = assign;
        issue.label          = label;
        issue.due_date       = due;
    }
    if let Some(issue) = ctx.issues.iter().find(|i| i.id == id).cloned() {
        set_active(&mut ctx, &issue);
    }
    update_stats(&mut ctx);
    recompute_filtered(&mut ctx);
    Ok(ctx)
}

fn cancel_edit(ctx: PlaneCtx, _: Value) -> Result<PlaneCtx, MachineError> { Ok(ctx) }

fn add_comment(mut ctx: PlaneCtx, p: Value) -> Result<PlaneCtx, MachineError> {
    let body = p["draft_comment"].as_str().unwrap_or("").trim().to_string();
    if body.is_empty() { return Ok(ctx); }
    let cid = next_cid(&ctx.active_comments);
    let comment = Comment { id: cid, author: "You".into(), body, ts: "just now".into() };
    let id = ctx.active_id.clone();
    if let Some(issue) = ctx.issues.iter_mut().find(|i| i.id == id) {
        issue.comments.push(comment.clone());
        issue.comment_count = issue.comments.len() as u32;
    }
    ctx.active_comments.push(comment);
    ctx.draft_comment = String::new();
    recompute_filtered(&mut ctx);
    Ok(ctx)
}

fn delete_issue(mut ctx: PlaneCtx, _: Value) -> Result<PlaneCtx, MachineError> {
    let id = ctx.active_id.clone();
    ctx.issues.retain(|i| i.id != id);
    ctx.active_id = String::new();
    update_stats(&mut ctx);
    recompute_filtered(&mut ctx);
    Ok(ctx)
}

// ── main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let issues = vec![
        build_issue(1, "Design new onboarding flow",
            "Redesign the 3-step onboarding wizard. Focus on reducing time-to-value for new signups. Consider progressive disclosure.",
            "in_progress", "high", "Alice", "ux", "2026-06-01",
            vec![
                c(1, "Bob",   "Looks great so far. Should we add a skip option for experienced users?", "2 days ago"),
                c(2, "Alice", "Good call. I'll add a 'Skip tour' link on step 1.", "1 day ago"),
            ]),
        build_issue(2, "Fix authentication timeout bug",
            "Users get logged out after 10 minutes regardless of activity. The session renewal call is firing too late.",
            "todo", "urgent", "Bob", "bug", "2026-05-20",
            vec![
                c(3, "Charlie", "Reproduced locally. The token refresh is delayed by 30s after inactivity.", "3 hours ago"),
            ]),
        build_issue(3, "Add CSV export to reports",
            "Users need to export filtered report data as CSV for offline analysis. Should respect the current view filters.",
            "backlog", "medium", "Charlie", "feature", "", vec![]),
        build_issue(4, "Update API documentation",
            "Docs are out of date post-v2 migration. Priority: auth endpoints, pagination format, and rate limit headers.",
            "todo", "low", "Alice", "docs", "2026-06-15", vec![]),
        build_issue(5, "Optimize dashboard query performance",
            "The main dashboard loads in 4-8 seconds for accounts with >1000 records. Target: under 800ms. Investigate N+1 queries.",
            "in_progress", "high", "Bob", "performance", "2026-05-28",
            vec![
                c(4, "Bob", "Found the culprit: the activity feed runs a separate query per user. Will batch.", "5 hours ago"),
            ]),
        build_issue(6, "Release v2.1.0",
            "Cut the release branch, update changelog, tag the release, notify customers.",
            "done", "high", "Charlie", "release", "2026-05-10",
            vec![
                c(5, "Alice",   "Changelog draft is ready for review.", "1 week ago"),
                c(6, "Bob",     "LGTM. Approving.", "1 week ago"),
                c(7, "Charlie", "Released. Monitoring error rates.", "6 days ago"),
            ]),
        build_issue(7, "Fix typo in settings page",
            "'Notifcations' → 'Notifications' on the account preferences screen.",
            "done", "low", "Alice", "bug", "", vec![]),
        build_issue(8, "Add dark mode support",
            "Implement a dark theme toggle. Store preference in localStorage. Use CSS custom properties for all colors.",
            "backlog", "medium", "Bob", "feature", "", vec![]),
        build_issue(9, "Investigate memory leak in background worker",
            "Heap grows ~50MB/hour in the job processor. Likely a retained event listener or unclosed DB connection.",
            "todo", "urgent", "Charlie", "bug", "2026-05-22", vec![]),
        build_issue(10, "Add keyboard shortcuts",
            "Power users want J/K navigation, C to create, E to edit, and ? for help. Modeled on Linear's shortcut system.",
            "backlog", "medium", "", "feature", "", vec![]),
    ];

    let mut ctx = PlaneCtx {
        filtered_issues: issues.clone(),
        next_seq: (issues.len() as u32) + 1,
        ..Default::default()
    };
    ctx.issues = issues;
    update_stats(&mut ctx);

    let initial = serde_json::to_value(ctx).expect("serialize initial ctx");

    let machine = MachineBuilder::new("plane", "list", initial)
        .state("filter")
        .state("detail")
        .state("create")
        .state("edit")
        // list transitions
        .typed_on("list",   "open_create",   "create", open_create)
        .typed_on("list",   "open_issue",    "detail", open_issue)
        .typed_on("list",   "toggle_filter", "filter", toggle_filter)
        // filter transitions
        .typed_on("filter", "open_issue",    "detail", open_issue)
        .typed_on("filter", "apply_filter",  "list",   apply_filter)
        .typed_on("filter", "clear_filter",  "list",   clear_filter)
        .typed_on("filter", "close_filter",  "list",   close_filter)
        // detail transitions
        .typed_on("detail", "back",          "list",   back)
        .typed_on("detail", "start_edit",    "edit",   start_edit)
        .typed_on("detail", "add_comment",   "detail", add_comment)
        .typed_on("detail", "delete_issue",  "list",   delete_issue)
        // create transitions
        .typed_on("create", "save_create",   "list",   save_create)
        .typed_on("create", "cancel_create", "list",   cancel_create)
        // edit transitions
        .typed_on("edit",   "save_edit",     "detail", save_edit)
        .typed_on("edit",   "cancel_edit",   "detail", cancel_edit)
        .template(page("Foster · Plane", include_str!("../static/style.css"), html! {
            div[machine="plane", class="app"] {

                // ── sidebar ───────────────────────────────────────────────────
                nav[class="sidebar"] {
                    div[class="sidebar-logo"] { "Plane" }
                    div[class="sidebar-project"] { "Acme Corp" }
                    div[class="sidebar-divider"]
                    a[class="nav-item nav-active"] { "Issues" }
                    a[class="nav-item"] { "Cycles" }
                    a[class="nav-item"] { "Modules" }
                    a[class="nav-item"] { "Views" }
                    a[class="nav-item"] { "Pages" }
                    div[class="sidebar-divider"]
                    a[class="nav-item"] { "Members" }
                    a[class="nav-item"] { "Settings" }
                }

                // ── main ──────────────────────────────────────────────────────
                div[class="main"] {

                    // topbar — always visible
                    div[class="topbar"] {
                        div[class="topbar-left"] {
                            span[class="topbar-title"] { "Issues" }
                            span[class="topbar-stats"] {
                                span[text="open_count"] { "0" }
                                " open  ·  "
                                span[text="done_count"] { "0" }
                                " done"
                            }
                        }
                        div[class="topbar-actions", show="list,filter"] {
                            button[class="btn btn-outline", on="click->toggle_filter", show="list"] {
                                "Filter"
                            }
                            button[class="btn btn-outline btn-filter-on", on="click->close_filter", show="filter"] {
                                "Filter (on)"
                            }
                            button[class="btn btn-primary", on="click->open_create"] {
                                "+ New issue"
                            }
                        }
                        div[class="topbar-actions", show="detail,edit"] {
                            button[class="btn btn-ghost", on="click->back"] { "← Back" }
                            button[class="btn btn-outline", on="click->start_edit", show="detail"] { "Edit" }
                            button[class="btn btn-ghost btn-danger", on="click->delete_issue", show="detail"] { "Delete" }
                        }
                    }

                    // ── list view (list + filter + create states) ─────────────
                    div[class="list-view", show="list,filter,create"] {

                        // filter panel
                        div[class="filter-panel", show="filter"] {
                            div[class="filter-heading"] { "Filter issues" }
                            div[class="filter-row"] {
                                label[class="filter-label"] { "Status" }
                                select[class="filter-select", collect="filter_status", value="filter_status"] {
                                    option[value=""] { "All statuses" }
                                    option[value="backlog"]     { "Backlog" }
                                    option[value="todo"]        { "To Do" }
                                    option[value="in_progress"] { "In Progress" }
                                    option[value="done"]        { "Done" }
                                    option[value="cancelled"]   { "Cancelled" }
                                }
                            }
                            div[class="filter-row"] {
                                label[class="filter-label"] { "Priority" }
                                select[class="filter-select", collect="filter_priority", value="filter_priority"] {
                                    option[value=""]       { "All priorities" }
                                    option[value="urgent"] { "Urgent" }
                                    option[value="high"]   { "High" }
                                    option[value="medium"] { "Medium" }
                                    option[value="low"]    { "Low" }
                                }
                            }
                            div[class="filter-row"] {
                                label[class="filter-label"] { "Assignee" }
                                input[type="text", class="filter-input", collect="filter_assignee",
                                      value="filter_assignee", placeholder="Name…"]
                            }
                            div[class="filter-actions"] {
                                button[class="btn btn-ghost", on="click->clear_filter"]  { "Clear" }
                                button[class="btn btn-primary", on="click->apply_filter"] { "Apply" }
                            }
                        }

                        // issue table
                        div[class="issue-table"] {
                            div[class="table-head"] {
                                span[class="col-st"]  { "Status" }
                                span[class="col-id"]  { "ID" }
                                span[class="col-pr"]  { "P" }
                                span[class="col-ti"]  { "Title" }
                                span[class="col-as"]  { "Assignee" }
                                span[class="col-lb"]  { "Label" }
                                span[class="col-du"]  { "Due" }
                            }
                            div[each="filtered_issues"] {
                                div[class="issue-row", on="click->open_issue", style="display:none"] {
                                    span[class="col-st status-icon", field="status_icon"]    { "○" }
                                    span[class="col-id dim",         field="seq_str"]        { "?" }
                                    span[class="col-pr dim pri-icon", field="priority_icon"] { "→" }
                                    span[class="col-ti",             field="title"]          { "…" }
                                    span[class="col-as dim",         field="assignee"]       { "" }
                                    span[class="col-lb dim label-chip", field="label"]       { "" }
                                    span[class="col-du dim",         field="due_date"]       { "" }
                                }
                            }
                        }
                    }

                    // ── detail view (detail + edit states) ───────────────────
                    div[class="detail-view", show="detail,edit"] {
                        div[class="detail-meta"] {
                            span[class="meta-chip"] {
                                span[class="meta-icon", text="active_status_icon"]    { "○" }
                                span[text="active_status_label"]   { "—" }
                            }
                            span[class="meta-chip"] {
                                span[class="meta-icon", text="active_priority_icon"]  { "→" }
                                span[text="active_priority_label"] { "—" }
                            }
                            span[class="meta-chip", show="detail"] {
                                span[class="meta-label"] { "Assignee " }
                                span[text="active_assignee"] { "—" }
                            }
                            span[class="meta-chip", show="detail"] {
                                span[class="meta-label"] { "Label " }
                                span[text="active_label"] { "—" }
                            }
                            span[class="meta-chip", show="detail"] {
                                span[class="meta-label"] { "Due " }
                                span[text="active_due_date"] { "—" }
                            }
                        }
                        div[class="detail-seq dim", text="active_seq_str"] { "EX-?" }
                        h2[class="detail-title", text="active_title"] { "…" }
                        p[class="detail-desc", text="active_description"] { "" }

                        // comments — only in detail state (edit modal covers it)
                        div[class="comments-section", show="detail"] {
                            div[class="comments-heading"] { "Activity" }
                            div[class="comments-list"] {
                                div[each="active_comments"] {
                                    div[class="comment", style="display:none"] {
                                        div[class="comment-meta"] {
                                            span[class="comment-author", field="author"] { "" }
                                            span[class="comment-ts dim",  field="ts"]     { "" }
                                        }
                                        p[class="comment-body", field="body"] { "" }
                                    }
                                }
                            }
                            div[class="comment-compose"] {
                                textarea[class="comment-input", placeholder="Add a comment…",
                                         collect="draft_comment", value="draft_comment"]
                                button[class="btn btn-primary", on="click->add_comment"] { "Comment" }
                            }
                        }
                    }
                }
            }

            // ── modals (create + edit) ────────────────────────────────────────
            div[class="modal-backdrop", show="create,edit"] {

                div[class="modal", show="create"] {
                    div[class="modal-header"] { "New issue" }
                    div[class="modal-body"] {
                        div[class="form-row"] {
                            label[class="form-label"] { "Title" }
                            input[type="text", class="form-input", collect="draft_title",
                                  placeholder="Issue title…"]
                        }
                        div[class="form-row"] {
                            label[class="form-label"] { "Description" }
                            textarea[class="form-textarea", collect="draft_description",
                                     placeholder="Add details…"]
                        }
                        div[class="form-grid"] {
                            div[class="form-row"] {
                                label[class="form-label"] { "Status" }
                                select[class="form-select", collect="draft_status", value="draft_status"] {
                                    option[value="backlog"]     { "Backlog" }
                                    option[value="todo"]        { "To Do" }
                                    option[value="in_progress"] { "In Progress" }
                                    option[value="done"]        { "Done" }
                                    option[value="cancelled"]   { "Cancelled" }
                                }
                            }
                            div[class="form-row"] {
                                label[class="form-label"] { "Priority" }
                                select[class="form-select", collect="draft_priority", value="draft_priority"] {
                                    option[value="urgent"] { "Urgent" }
                                    option[value="high"]   { "High" }
                                    option[value="medium"] { "Medium" }
                                    option[value="low"]    { "Low" }
                                    option[value="none"]   { "None" }
                                }
                            }
                            div[class="form-row"] {
                                label[class="form-label"] { "Assignee" }
                                select[class="form-select", collect="draft_assignee", value="draft_assignee"] {
                                    option[value=""]        { "Unassigned" }
                                    option[value="Alice"]   { "Alice" }
                                    option[value="Bob"]     { "Bob" }
                                    option[value="Charlie"] { "Charlie" }
                                }
                            }
                            div[class="form-row"] {
                                label[class="form-label"] { "Label" }
                                select[class="form-select", collect="draft_label", value="draft_label"] {
                                    option[value=""]            { "No label" }
                                    option[value="bug"]         { "Bug" }
                                    option[value="feature"]     { "Feature" }
                                    option[value="improvement"] { "Improvement" }
                                    option[value="docs"]        { "Docs" }
                                    option[value="performance"] { "Performance" }
                                    option[value="release"]     { "Release" }
                                    option[value="ux"]          { "UX" }
                                }
                            }
                        }
                        div[class="form-row"] {
                            label[class="form-label"] { "Due date" }
                            input[type="date", class="form-input", collect="draft_due_date"]
                        }
                    }
                    div[class="modal-footer"] {
                        button[class="btn btn-ghost", on="click->cancel_create"] { "Cancel" }
                        button[class="btn btn-primary", on="click->save_create"]  { "Create issue" }
                    }
                }

                div[class="modal", show="edit"] {
                    div[class="modal-header"] { "Edit issue" }
                    div[class="modal-body"] {
                        div[class="form-row"] {
                            label[class="form-label"] { "Title" }
                            input[type="text", class="form-input", collect="draft_title",
                                  value="draft_title", placeholder="Issue title…"]
                        }
                        div[class="form-row"] {
                            label[class="form-label"] { "Description" }
                            textarea[class="form-textarea", collect="draft_description",
                                     value="draft_description", placeholder="Add details…"]
                        }
                        div[class="form-grid"] {
                            div[class="form-row"] {
                                label[class="form-label"] { "Status" }
                                select[class="form-select", collect="draft_status", value="draft_status"] {
                                    option[value="backlog"]     { "Backlog" }
                                    option[value="todo"]        { "To Do" }
                                    option[value="in_progress"] { "In Progress" }
                                    option[value="done"]        { "Done" }
                                    option[value="cancelled"]   { "Cancelled" }
                                }
                            }
                            div[class="form-row"] {
                                label[class="form-label"] { "Priority" }
                                select[class="form-select", collect="draft_priority", value="draft_priority"] {
                                    option[value="urgent"] { "Urgent" }
                                    option[value="high"]   { "High" }
                                    option[value="medium"] { "Medium" }
                                    option[value="low"]    { "Low" }
                                    option[value="none"]   { "None" }
                                }
                            }
                            div[class="form-row"] {
                                label[class="form-label"] { "Assignee" }
                                select[class="form-select", collect="draft_assignee", value="draft_assignee"] {
                                    option[value=""]        { "Unassigned" }
                                    option[value="Alice"]   { "Alice" }
                                    option[value="Bob"]     { "Bob" }
                                    option[value="Charlie"] { "Charlie" }
                                }
                            }
                            div[class="form-row"] {
                                label[class="form-label"] { "Label" }
                                select[class="form-select", collect="draft_label", value="draft_label"] {
                                    option[value=""]            { "No label" }
                                    option[value="bug"]         { "Bug" }
                                    option[value="feature"]     { "Feature" }
                                    option[value="improvement"] { "Improvement" }
                                    option[value="docs"]        { "Docs" }
                                    option[value="performance"] { "Performance" }
                                    option[value="release"]     { "Release" }
                                    option[value="ux"]          { "UX" }
                                }
                            }
                        }
                        div[class="form-row"] {
                            label[class="form-label"] { "Due date" }
                            input[type="date", class="form-input", collect="draft_due_date",
                                  value="draft_due_date"]
                        }
                    }
                    div[class="modal-footer"] {
                        button[class="btn btn-ghost",   on="click->cancel_edit"] { "Cancel" }
                        button[class="btn btn-primary", on="click->save_edit"]   { "Save changes" }
                    }
                }
            }
        }))
        .build();

    let mut machines = HashMap::new();
    machines.insert("plane".to_string(), machine);

    let pkg_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg");
    let app = foster_server::router(machines)
        .nest_service("/pkg", ServeDir::new(pkg_dir));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3005").await.unwrap();
    println!("Foster plane → http://localhost:3005");
    axum::serve(listener, app).await.unwrap();
}
