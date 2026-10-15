//! Client-side list views: filter, sort, selection and keyboard focus for
//! an `fx-for` list, kept in the browser — no server round trip, no server
//! state. The server sends the rows; how they're looked at is the page's.
//!
//! ```html
//! <input fx-view-filter="files">
//! <button fx-view-sort="files:name">Name <span fx-text="$files.sort_name"></span></button>
//! <main fx-view-keys="files" fx-view-payload="files" fx-on="keydown.Enter->activate">
//!   <div fx-for="entries" fx-key="path" fx-view="files" fx-view-group="is_dir"
//!        fx-view-filter-fields="name" fx-view-reset="path epoch">
//!     <div><button fx-view-select="files"></button> …</div>
//!   </div>
//! </main>
//! <span fx-text="$files.selected"></span> selected
//! ```
//!
//! - `fx-view="name"` on an `fx-for` (with `fx-key`) shows its rows filtered
//!   (every word of the filter in one of `fx-view-filter-fields`, default
//!   `name`) and sorted (`fx-view-group="field"`: rows with that field truthy
//!   first), and marks rows `selected` / `focused`.
//! - `fx-view-reset="a b"`: context fields; when one changes (another folder,
//!   a bulk action done) the filter, selection and focus reset.
//! - Controls: `fx-view-filter`, `fx-view-sort="name:field"` (again flips
//!   direction), `fx-view-select` in a row (shift-click: range),
//!   `fx-view-select-all`, `fx-view-clear`, `fx-view-keys` (↑/↓ move focus).
//! - `$name.field` in any binding: `shown`, `total`, `selected`,
//!   `all_selected`, `filtered`, `filter`, `sort_<field>` (▲/▼ or "").
//! - Events from inside `fx-view-payload="name"` carry `selected` (keys, in
//!   view order) and `focused` (the row's item, or null).
//! - `fx-view-step="name:1"` (or `:-1`) with `fx-view-from="ctx field"`
//!   (the current row's key) and optional `fx-view-where="item field"`: the
//!   element is `disabled` when there's no such neighbour, and an event it
//!   fires carries the neighbour's item. `fx-view-pos` (same attributes,
//!   no step) shows "3 of 12".
//! - `fx-view-inputs="name"` with `data-name="field"`: hidden inputs, one per
//!   selected key, for a plain `<form>`.
//!
//! State is per tab (sessionStorage), so a reload keeps it.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement, HtmlInputElement};

#[derive(Default, Clone, Serialize, Deserialize)]
struct ViewState {
    filter: String,
    sort: Option<String>,
    desc: bool,
    selected: Vec<String>,
    focus: Option<String>,
    anchor: Option<String>,
    reset_sig: Option<String>,
}

/// The rows a view shows right now, in order, and how many it has in all.
#[derive(Default)]
struct Shown {
    rows: Vec<(String, Value)>,
    total: usize,
}

thread_local! {
    static STATES: RefCell<HashMap<String, ViewState>> = RefCell::new(HashMap::new());
    /// view name → list (its `fx-for` key) → what it shows. Several lists can
    /// share a view (a list and a grid of the same rows, one of them empty).
    static SHOWN: RefCell<HashMap<String, HashMap<String, Shown>>> = RefCell::new(HashMap::new());
    /// view name → (the `fx-view-from` value when a step was clicked, the
    /// row it stepped to): until the context catches up, further steps go
    /// on from that row — a quick second ← isn't the same step again.
    static STEPPED: RefCell<HashMap<String, (String, String)>> = RefCell::new(HashMap::new());
}

/// Run `f` on what view `name` shows: its list with the most rows.
fn with_shown<R>(name: &str, f: impl FnOnce(Option<&Shown>) -> R) -> R {
    SHOWN.with(|s| {
        let s = s.borrow();
        f(s.get(name).and_then(|lists| lists.values().max_by_key(|v| v.total)))
    })
}

fn shown_keys(name: &str) -> Vec<String> {
    with_shown(name, |v| v.map(|v| v.rows.iter().map(|(k, _)| k.clone()).collect()).unwrap_or_default())
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.session_storage().ok()?
}

fn state(name: &str) -> ViewState {
    STATES.with(|s| {
        s.borrow_mut()
            .entry(name.to_string())
            .or_insert_with(|| {
                storage()
                    .and_then(|st| st.get_item(&format!("foster_view:{name}")).ok()?)
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or_default()
            })
            .clone()
    })
}

fn update(name: &str, f: impl FnOnce(&mut ViewState)) {
    let mut st = state(name);
    f(&mut st);
    if let (Some(storage), Ok(raw)) = (storage(), serde_json::to_string(&st)) {
        let _ = storage.set_item(&format!("foster_view:{name}"), &raw);
    }
    STATES.with(|s| s.borrow_mut().insert(name.to_string(), st));
}

/// A row's key: its `fx-key` field (else its position).
pub fn key_of(key_field: Option<&str>, item: &Value, i: usize) -> String {
    match key_field.and_then(|f| item.get(f)) {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => format!("#{i}"),
    }
}

fn truthy(v: Option<&Value>) -> bool {
    !matches!(v, None | Some(Value::Null | Value::Bool(false))) && v != Some(&json!("")) && v != Some(&json!(0))
}

/// Order two field values: numbers numerically, anything else as
/// case-insensitive text; missing last.
fn compare(a: Option<&Value>, b: Option<&Value>) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a, b) {
        (None | Some(Value::Null), None | Some(Value::Null)) => Equal,
        (None | Some(Value::Null), _) => Greater,
        (_, None | Some(Value::Null)) => Less,
        (Some(Value::Number(x)), Some(Value::Number(y))) => {
            x.as_f64().unwrap_or(0.0).partial_cmp(&y.as_f64().unwrap_or(0.0)).unwrap_or(Equal)
        }
        (Some(x), Some(y)) => text(x).to_lowercase().cmp(&text(y).to_lowercase()),
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// For an `fx-for` container with `fx-view`: the items to show (filtered and
/// sorted), recorded for the controls and `$name` bindings. Resets the view
/// when a `fx-view-reset` field changed.
pub fn items(container: &Element, ctx: &Value, all: &[Value]) -> Vec<Value> {
    let Some(name) = container.get_attribute("fx-view") else { return all.to_vec() };
    let key_field = container.get_attribute("fx-key");

    if let Some(fields) = container.get_attribute("fx-view-reset") {
        let sig: Vec<String> = fields.split_whitespace().map(|f| ctx.get(f).map(text).unwrap_or_default()).collect();
        let sig = sig.join("\u{1f}");
        let st = state(&name);
        if st.reset_sig.as_deref() != Some(sig.as_str()) {
            let had = st.reset_sig.is_some();
            update(&name, |st| {
                if had {
                    st.filter.clear();
                    st.selected.clear();
                    st.focus = None;
                    st.anchor = None;
                }
                st.reset_sig = Some(sig);
            });
        }
    }

    // Drop selected keys that aren't listed any more — judged by a list
    // that has rows (the other of a list/grid pair is empty).
    let keys: HashSet<String> = all.iter().enumerate().map(|(i, it)| key_of(key_field.as_deref(), it, i)).collect();
    let st = state(&name);
    if !all.is_empty() && (st.selected.iter().any(|k| !keys.contains(k)) || st.focus.as_ref().is_some_and(|f| !keys.contains(f))) {
        update(&name, |st| {
            st.selected.retain(|k| keys.contains(k));
            if st.focus.as_ref().is_some_and(|f| !keys.contains(f)) {
                st.focus = None;
            }
        });
    }
    let st = state(&name);

    let fields: Vec<String> = container
        .get_attribute("fx-view-filter-fields")
        .unwrap_or_else(|| "name".into())
        .split(',')
        .map(|f| f.trim().to_string())
        .collect();
    let words: Vec<String> = st.filter.to_lowercase().split_whitespace().map(String::from).collect();
    let mut rows: Vec<(String, Value)> = all
        .iter()
        .enumerate()
        .filter(|(_, it)| {
            words.is_empty()
                || fields.iter().any(|f| {
                    let t = it.get(f).map(text).unwrap_or_default().to_lowercase();
                    words.iter().all(|w| t.contains(w.as_str()))
                })
        })
        .map(|(i, it)| (key_of(key_field.as_deref(), it, i), it.clone()))
        .collect();

    let group = container.get_attribute("fx-view-group");
    rows.sort_by(|(_, a), (_, b)| {
        let grouped = match &group {
            Some(g) => truthy(b.get(g)).cmp(&truthy(a.get(g))),
            None => std::cmp::Ordering::Equal,
        };
        grouped.then_with(|| match &st.sort {
            Some(f) => {
                let o = compare(a.get(f), b.get(f));
                if st.desc { o.reverse() } else { o }
            }
            None => std::cmp::Ordering::Equal,
        })
    });

    let out = rows.iter().map(|(_, v)| v.clone()).collect();
    let list = container.get_attribute("fx-for").unwrap_or_default();
    SHOWN.with(|s| s.borrow_mut().entry(name).or_default().insert(list, Shown { rows, total: all.len() }));
    out
}

/// After a view's rows are rendered: mark them `selected` / `focused`.
pub fn mark_rows(container: &Element) {
    let Some(name) = container.get_attribute("fx-view") else { return };
    let st = state(&name);
    let selected: HashSet<&str> = st.selected.iter().map(String::as_str).collect();
    let mut child = container.first_element_child();
    while let Some(el) = child {
        child = el.next_element_sibling();
        let key = el.get_attribute("data-fx-key").unwrap_or_default();
        let cl = el.class_list();
        for (class, on) in [("selected", selected.contains(key.as_str())), ("focused", st.focus.as_deref() == Some(key.as_str()))] {
            if cl.contains(class) != on {
                let _ = cl.toggle_with_force(class, on);
            }
        }
    }
}

/// `$name.field` for bindings; `None` for keys that aren't view keys.
pub fn lookup(key: &str) -> Option<Value> {
    let (name, field) = key.strip_prefix('$')?.split_once('.')?;
    let st = state(name);
    let (shown, total, all_selected) = with_shown(name, |v| {
        let shown = v.map_or(0, |v| v.rows.len());
        let all = v.is_some_and(|v| !v.rows.is_empty() && v.rows.iter().all(|(k, _)| st.selected.contains(k)));
        (shown, v.map_or(0, |v| v.total), all)
    });
    Some(match field {
        "shown" => json!(shown),
        "total" => json!(total),
        "selected" => json!(st.selected.len()),
        "all_selected" => json!(all_selected),
        "filtered" => json!(shown != total),
        "filter" => json!(st.filter),
        f => match f.strip_prefix("sort_") {
            Some(col) if st.sort.as_deref() == Some(col) => json!(if st.desc { "▼" } else { "▲" }),
            Some(_) => json!(""),
            None => Value::Null,
        },
    })
}

/// `selected` / `focused` for an event fired inside `fx-view-payload`.
pub fn payload(el: &Element) -> Option<(Value, Value)> {
    let holder = el.closest("[fx-view-payload]").ok()??;
    let name = holder.get_attribute("fx-view-payload")?;
    let st = state(&name);
    let (selected, focused) = with_shown(&name, |v| {
        let rows = v.map(|v| v.rows.as_slice()).unwrap_or(&[]);
        // Selection in view order, then any selected rows the filter hides.
        let mut keys: Vec<String> = rows.iter().filter(|(k, _)| st.selected.contains(k)).map(|(k, _)| k.clone()).collect();
        let hidden: Vec<String> = st.selected.iter().filter(|k| !keys.contains(k)).cloned().collect();
        keys.extend(hidden);
        let focused = st.focus.as_ref().and_then(|f| rows.iter().find(|(k, _)| k == f)).map_or(Value::Null, |(_, v)| v.clone());
        (keys, focused)
    });
    Some((json!(selected), focused))
}

/// Render pass for a machine root: `fx-view-step` / `fx-view-pos`
/// elements, view filter inputs, and `fx-view-inputs`.
pub fn render(document: &Document, root: &Element, ctx: &Value) {
    for el in crate::render_targets(root, "[fx-view-step]").into_iter().chain(crate::render_targets(root, "[fx-view-pos]")) {
        let spec = el.get_attribute("fx-view-step").or_else(|| el.get_attribute("fx-view-pos")).unwrap_or_default();
        let (name, by) = spec.split_once(':').map_or((spec.as_str(), 0), |(n, b)| (n, b.parse::<i64>().unwrap_or(0)));
        let at_ctx = el.get_attribute("fx-view-from").and_then(|f| ctx.get(&f).map(text));
        let from = STEPPED.with(|st| {
            let mut st = st.borrow_mut();
            match st.get(name) {
                Some((was, to)) if Some(was) == at_ctx.as_ref() => Some(to.clone()),
                Some(_) => {
                    st.remove(name);
                    at_ctx.clone()
                }
                None => at_ctx.clone(),
            }
        });
        let wanted = el.get_attribute("fx-view-where");
        let (pos, count, neighbour, neighbour_key) = with_shown(name, |v| {
            let rows: Vec<&(String, Value)> = v
                .map(|v| v.rows.iter().filter(|(_, it)| wanted.as_ref().is_none_or(|w| truthy(it.get(w)))).collect())
                .unwrap_or_default();
            let at = from.as_ref().and_then(|f| rows.iter().position(|(k, _)| k == f));
            let next = at.and_then(|i| usize::try_from(i as i64 + by).ok().filter(|_| by != 0)).and_then(|j| rows.get(j));
            (at, rows.len(), next.map(|(_, v)| v.clone()), next.map(|(k, _)| k.clone()))
        });
        if el.has_attribute("fx-view-pos") {
            el.set_text_content(Some(&pos.map_or(String::new(), |i| format!("{} of {count}", i + 1))));
            continue;
        }
        match (neighbour, neighbour_key) {
            (Some(item), Some(key)) => {
                let _ = el.remove_attribute("disabled");
                let _ = el.set_attribute("data-fx-step-item", &item.to_string());
                let _ = el.set_attribute("data-fx-step-key", &key);
                let _ = el.set_attribute("data-fx-step-at", at_ctx.as_deref().unwrap_or(""));
            }
            _ => {
                let _ = el.set_attribute("disabled", "");
                let _ = el.remove_attribute("data-fx-step-item");
            }
        }
    }
    for el in crate::render_targets(root, "[fx-view-filter]") {
        let name = el.get_attribute("fx-view-filter").unwrap_or_default();
        if let Some(input) = el.dyn_ref::<HtmlInputElement>() {
            let want = state(&name).filter;
            if input.value() != want {
                input.set_value(&want);
            }
        }
    }
    for el in crate::render_targets(root, "[fx-view-inputs]") {
        let name = el.get_attribute("fx-view-inputs").unwrap_or_default();
        let field = el.get_attribute("data-name").unwrap_or_else(|| "key".into());
        el.set_inner_html("");
        for key in state(&name).selected {
            if let Ok(input) = document.create_element("input") {
                let _ = input.set_attribute("type", "hidden");
                let _ = input.set_attribute("name", &field);
                let _ = input.set_attribute("value", &key);
                let _ = el.append_child(&input);
            }
        }
    }
}

/// Handle a DOM event for the view controls; true if it changed a view
/// (the page should re-render).
pub fn handle(event: &web_sys::Event) -> bool {
    let Some(target) = event.target().and_then(|t| t.dyn_into::<Element>().ok()) else { return false };
    let kind = event.type_();
    let closest = |sel: &str| target.closest(sel).ok().flatten();

    if kind == "input" {
        if let Some(el) = closest("[fx-view-filter]") {
            let name = el.get_attribute("fx-view-filter").unwrap_or_default();
            let value = el.dyn_ref::<HtmlInputElement>().map(|i| i.value()).unwrap_or_default();
            update(&name, |st| {
                st.filter = value;
                st.focus = None;
            });
            return true;
        }
        return false;
    }
    if kind == "click" {
        let shift = event.dyn_ref::<web_sys::MouseEvent>().is_some_and(|m| m.shift_key());
        // A step was taken (its own fx-on sends the neighbour): step on from
        // there until the context shows it.
        if let Some(el) = closest("[fx-view-step][data-fx-step-key]") {
            let name = el.get_attribute("fx-view-step").unwrap_or_default();
            let name = name.split(':').next().unwrap_or_default().to_string();
            let at = el.get_attribute("data-fx-step-at").unwrap_or_default();
            let to = el.get_attribute("data-fx-step-key").unwrap_or_default();
            STEPPED.with(|st| st.borrow_mut().insert(name, (at, to)));
            return true;
        }
        if let Some(el) = closest("[fx-view-sort]") {
            let spec = el.get_attribute("fx-view-sort").unwrap_or_default();
            let Some((name, field)) = spec.split_once(':') else { return false };
            let field = field.to_string();
            update(name, |st| {
                if st.sort.as_deref() == Some(field.as_str()) {
                    st.desc = !st.desc;
                } else {
                    st.sort = Some(field);
                    st.desc = false;
                }
            });
            return true;
        }
        if let Some(el) = closest("[fx-view-select]") {
            let name = el.get_attribute("fx-view-select").unwrap_or_default();
            let Some(key) = el.closest("[data-fx-key]").ok().flatten().and_then(|r| r.get_attribute("data-fx-key")) else { return false };
            let order: Vec<String> = shown_keys(&name);
            update(&name, |st| {
                let range = shift.then(|| st.anchor.as_ref().and_then(|a| order.iter().position(|k| k == a))).flatten();
                match (range, order.iter().position(|k| *k == key)) {
                    (Some(a), Some(b)) => {
                        for k in &order[a.min(b)..=a.max(b)] {
                            if !st.selected.contains(k) {
                                st.selected.push(k.clone());
                            }
                        }
                    }
                    _ => match st.selected.iter().position(|k| *k == key) {
                        Some(i) => {
                            st.selected.remove(i);
                        }
                        None => st.selected.push(key.clone()),
                    },
                }
                st.anchor = Some(key);
            });
            return true;
        }
        if let Some(el) = closest("[fx-view-select-all]") {
            let name = el.get_attribute("fx-view-select-all").unwrap_or_default();
            let shown: Vec<String> = shown_keys(&name);
            update(&name, |st| {
                if !shown.is_empty() && shown.iter().all(|k| st.selected.contains(k)) {
                    st.selected.retain(|k| !shown.contains(k));
                } else {
                    for k in shown {
                        if !st.selected.contains(&k) {
                            st.selected.push(k);
                        }
                    }
                }
            });
            return true;
        }
        if let Some(el) = closest("[fx-view-clear]") {
            let name = el.get_attribute("fx-view-clear").unwrap_or_default();
            update(&name, |st| st.selected.clear());
            return true;
        }
        return false;
    }
    if kind == "keydown" {
        let Some(key) = event.dyn_ref::<web_sys::KeyboardEvent>().map(|k| k.key()) else { return false };
        let by: i64 = match key.as_str() {
            "ArrowDown" => 1,
            "ArrowUp" => -1,
            _ => return false,
        };
        if crate::typing_in_field(event) {
            return false;
        }
        let Some(el) = closest("[fx-view-keys]") else { return false };
        let name = el.get_attribute("fx-view-keys").unwrap_or_default();
        let order: Vec<String> = shown_keys(&name);
        if order.is_empty() {
            return false;
        }
        event.prevent_default();
        update(&name, |st| {
            let at = st.focus.as_ref().and_then(|f| order.iter().position(|k| k == f));
            let next = match at {
                None => 0,
                Some(i) => (i as i64 + by).clamp(0, order.len() as i64 - 1) as usize,
            };
            st.focus = Some(order[next].clone());
        });
        return true;
    }
    false
}

/// Scroll each view's focused row into view (after keyboard moves).
pub fn reveal_focus(document: &Document) {
    let Ok(lists) = document.query_selector_all("[fx-view]") else { return };
    for i in 0..lists.length() {
        let Some(list) = lists.item(i).and_then(|n| n.dyn_into::<Element>().ok()) else { continue };
        let Some(focus) = list.get_attribute("fx-view").and_then(|n| state(&n).focus) else { continue };
        let mut child = list.first_element_child();
        while let Some(el) = child {
            if el.get_attribute("data-fx-key").as_deref() == Some(focus.as_str()) {
                if let Some(h) = el.dyn_ref::<HtmlElement>() {
                    let opts = web_sys::ScrollIntoViewOptions::new();
                    opts.set_block(web_sys::ScrollLogicalPosition::Nearest);
                    h.scroll_into_view_with_scroll_into_view_options(&opts);
                }
                break;
            }
            child = el.next_element_sibling();
        }
    }
}
