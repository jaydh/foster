use foster_core::Snapshot;
use serde_json::Value;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{Document, Element, HtmlElement, Request, RequestInit, RequestMode, Response};

// ── entry point ──────────────────────────────────────────────────────────────

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    spawn_local(bootstrap());
}

async fn bootstrap() {
    let window = web_sys::window().expect("no window");
    let document = window.document().expect("no document");

    let roots = document.query_selector_all("[fx-machine]").unwrap();
    for i in 0..roots.length() {
        let root: Element = roots.item(i).unwrap().dyn_into().unwrap();
        let machine_id = root.get_attribute("fx-machine").unwrap();

        // Snapshot inner HTML of all fx-for containers BEFORE the first render,
        // so we always have a clean item template to clone from.
        save_for_templates(&root);

        match fetch_snapshot(&machine_id).await {
            Ok(snap) => {
                apply_snapshot(&document, &root, &snap);
                update_debug(&document, &snap);
                // One delegating click listener covers static elements AND
                // dynamically rendered fx-for items without re-wiring.
                attach_delegating_listener(document.clone(), root, machine_id);
            }
            Err(e) => web_sys::console::error_1(&e),
        }
    }
}

// ── snapshot snapshot application ────────────────────────────────────────────

fn apply_snapshot(document: &Document, root: &Element, snap: &Snapshot) {
    root.set_attribute("data-fx-state", &snap.state).unwrap();
    root.set_attribute("data-fx-version", &snap.version.to_string()).unwrap();

    apply_fx_show(root, &snap.state);
    apply_fx_text(root, &snap.context);
    apply_fx_disable(root, &snap.state);
    apply_fx_state_label(root, &snap.state);
    apply_fx_value(root, &snap.context);
    apply_fx_for(document, root, &snap.context);
}

// ── attribute processors ─────────────────────────────────────────────────────

/// `fx-show="idle,loading"` — show only in listed states.
fn apply_fx_show(root: &Element, state: &str) {
    let els = root.query_selector_all("[fx-show]").unwrap();
    for i in 0..els.length() {
        let el: HtmlElement = els.item(i).unwrap().dyn_into().unwrap();
        let attr = el.get_attribute("fx-show").unwrap_or_default();
        let visible = attr.split(',').any(|s| s.trim() == state);
        el.style()
            .set_property("display", if visible { "" } else { "none" })
            .unwrap();
    }
}

/// `fx-text="count"` — set text from a top-level context key.
fn apply_fx_text(root: &Element, ctx: &Value) {
    let els = root.query_selector_all("[fx-text]").unwrap();
    for i in 0..els.length() {
        let el: Element = els.item(i).unwrap().dyn_into().unwrap();
        let key = el.get_attribute("fx-text").unwrap_or_default();
        if let Some(val) = ctx.get(&key) {
            el.set_text_content(Some(&val_to_string(val)));
        }
    }
}

/// `fx-disable="loading,saving"` — add `disabled` attribute in listed states.
fn apply_fx_disable(root: &Element, state: &str) {
    let els = root.query_selector_all("[fx-disable]").unwrap();
    for i in 0..els.length() {
        let el: Element = els.item(i).unwrap().dyn_into().unwrap();
        let attr = el.get_attribute("fx-disable").unwrap_or_default();
        if attr.split(',').any(|s| s.trim() == state) {
            el.set_attribute("disabled", "").unwrap();
        } else {
            el.remove_attribute("disabled").unwrap();
        }
    }
}

/// `fx-state-label` — display current state name (dev/debug).
fn apply_fx_state_label(root: &Element, state: &str) {
    let els = root.query_selector_all("[fx-state-label]").unwrap();
    for i in 0..els.length() {
        let el: Element = els.item(i).unwrap().dyn_into().unwrap();
        el.set_text_content(Some(state));
    }
}

/// `fx-value="email"` — populate an input with a context value.
/// Used to restore form fields when navigating back through a wizard.
fn apply_fx_value(root: &Element, ctx: &Value) {
    let els = root.query_selector_all("[fx-value]").unwrap();
    for i in 0..els.length() {
        let el: Element = els.item(i).unwrap().dyn_into().unwrap();
        let key = el.get_attribute("fx-value").unwrap_or_default();
        if let Some(val) = ctx.get(&key) {
            let text = val_to_string(val);
            if let Some(input) = el.dyn_ref::<web_sys::HtmlInputElement>() {
                input.set_value(&text);
            } else if let Some(ta) = el.dyn_ref::<web_sys::HtmlTextAreaElement>() {
                ta.set_value(&text);
            }
        }
    }
}

/// `fx-for="tasks"` — re-render a list from a context array.
///
/// Optional: `fx-where='{"column":"todo"}'` filters to items matching all key/value pairs.
///
/// The container's initial inner HTML is saved as the item template the first time
/// `save_for_templates` runs (before any snapshot is applied).  On each snapshot,
/// the container is cleared and one clone of the template is inserted per item.
///
/// Within each clone:
///   • `fx-field="title"` sets text content from the item object
///   • `data-fx-item='{...}'` is stamped on the root clone for payload pickup
fn apply_fx_for(document: &Document, root: &Element, ctx: &Value) {
    let containers = root.query_selector_all("[fx-for]").unwrap();
    for i in 0..containers.length() {
        let container: Element = containers.item(i).unwrap().dyn_into().unwrap();
        let key = container.get_attribute("fx-for").unwrap_or_default();

        let Some(full_array) = ctx.get(&key).and_then(|v| v.as_array()) else {
            continue;
        };

        // fx-where='{"column":"todo"}' — filter to matching items only
        let where_filter: Option<serde_json::Map<String, Value>> = container
            .get_attribute("fx-where")
            .and_then(|s| serde_json::from_str(&s).ok());

        let array: Vec<Value> = full_array
            .iter()
            .filter(|item| match &where_filter {
                None => true,
                Some(filter) => filter
                    .iter()
                    .all(|(k, v)| item.get(k).map(|iv| iv == v).unwrap_or(false)),
            })
            .cloned()
            .collect();

        let Some(template_html) = container.get_attribute("data-fx-template-html") else {
            continue; // save_for_templates hasn't run yet
        };

        // Clear rendered items (everything inside the container).
        container.set_inner_html("");

        for item in &array {
            // Parse template into a real element via a scratch div.
            let scratch = document.create_element("div").unwrap();
            scratch.set_inner_html(&template_html);
            let Some(item_el) = scratch.first_element_child() else {
                continue;
            };

            // Stamp the full item JSON so click handlers can read it.
            item_el
                .set_attribute("data-fx-item", &item.to_string())
                .unwrap();

            // Show clones regardless of any display:none on the template.
            if let Ok(el) = item_el.clone().dyn_into::<HtmlElement>() {
                let _ = el.style().remove_property("display");
            }

            // Bind fx-field elements within the clone.
            apply_fx_fields(&item_el, item);

            container.append_child(&item_el).unwrap();
        }
    }
}

/// Save the initial inner HTML of every `[fx-for]` container as a template attribute.
/// Must be called once, before the first `apply_snapshot`.
fn save_for_templates(root: &Element) {
    let containers = root.query_selector_all("[fx-for]").unwrap();
    for i in 0..containers.length() {
        let container: Element = containers.item(i).unwrap().dyn_into().unwrap();
        if container.get_attribute("data-fx-template-html").is_none() {
            let html = container.inner_html();
            container
                .set_attribute("data-fx-template-html", &html)
                .unwrap();
        }
    }
}

/// Bind `fx-field="key"` text inside a list item clone.
fn apply_fx_fields(root: &Element, item: &Value) {
    // Root itself
    if let Some(key) = root.get_attribute("fx-field") {
        if let Some(val) = item.get(&key) {
            root.set_text_content(Some(&val_to_string(val)));
        }
    }
    // Descendants
    let els = root.query_selector_all("[fx-field]").unwrap();
    for i in 0..els.length() {
        let el: Element = els.item(i).unwrap().dyn_into().unwrap();
        let key = el.get_attribute("fx-field").unwrap_or_default();
        if let Some(val) = item.get(&key) {
            el.set_text_content(Some(&val_to_string(val)));
        }
    }
}

// ── event delegation ──────────────────────────────────────────────────────────
//
// A single click listener on the machine root handles all `[fx-on="click->..."]`
// elements — including dynamically rendered `fx-for` items.  No per-element
// wiring needed, nothing to re-attach after list re-renders.

fn attach_delegating_listener(document: Document, root: Element, machine_id: String) {
    let root_for_listener = root.clone();
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
        let root = &root_for_listener;
        // Find the [fx-on] ancestor of whatever was actually clicked.
        let Some(target) = event.target() else { return };
        let Ok(target_el): Result<Element, _> = target.dyn_into() else { return };

        let Some(fx_on_el) = find_fx_on_ancestor(&target_el, &root) else {
            return;
        };

        let fx_on = fx_on_el.get_attribute("fx-on").unwrap_or_default();
        let mut parts = fx_on.splitn(2, "->").map(|s| s.trim().to_string());
        let (Some(dom_event), Some(machine_event)) = (parts.next(), parts.next()) else {
            return;
        };

        // Only fire for the event type declared in the attribute.
        if event.type_() != dom_event {
            return;
        }

        let payload = build_payload(&fx_on_el, root);
        let mid = machine_id.clone();
        let root_clone = root.clone();
        let doc_clone = document.clone();

        spawn_local(async move {
            match send_transition(&mid, &machine_event, payload).await {
                Ok(snap) => {
                    apply_snapshot(&doc_clone, &root_clone, &snap);
                    update_debug(&doc_clone, &snap);
                }
                Err(e) => web_sys::console::error_1(&e),
            }
        });
    });

    root.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref())
        .unwrap();
    cb.forget(); // page-lifetime singleton
}

/// Walk up the DOM from `el` toward `root`, returning the first `[fx-on]` ancestor.
fn find_fx_on_ancestor(el: &Element, root: &Element) -> Option<Element> {
    let mut cur: Option<Element> = Some(el.clone());
    while let Some(c) = cur {
        if c.has_attribute("fx-on") {
            return Some(c);
        }
        if c == *root {
            return None;
        }
        cur = c.parent_element();
    }
    None
}

/// Build the transition payload from three sources (later sources take priority):
///   1. `[fx-collect]` input values within the machine root
///   2. `data-fx-item` JSON from the nearest list-item ancestor (kanban, etc.)
///   3. `fx-payload='{"key":"val"}'` static JSON on the clicked element
fn build_payload(fx_on_el: &Element, root: &Element) -> Value {
    let mut map = serde_json::Map::new();

    // 1. Collected form fields
    let els = root.query_selector_all("[fx-collect]").unwrap();
    for i in 0..els.length() {
        let el: Element = els.item(i).unwrap().dyn_into().unwrap();
        let key = el.get_attribute("fx-collect").unwrap_or_default();
        if let Some(val) = read_input_value(&el) {
            map.insert(key, Value::String(val));
        }
    }

    // 2. Nearest data-fx-item ancestor (from fx-for list rendering)
    if let Some(item_json) = find_item_ancestor(fx_on_el, root) {
        if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&item_json) {
            for (k, v) in obj {
                map.entry(k).or_insert(v);
            }
        }
    }

    // 3. Static fx-payload on the element (highest priority)
    if let Some(payload_str) = fx_on_el.get_attribute("fx-payload") {
        if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&payload_str) {
            for (k, v) in obj {
                map.insert(k, v);
            }
        }
    }

    if map.is_empty() {
        Value::Null
    } else {
        Value::Object(map)
    }
}

fn read_input_value(el: &Element) -> Option<String> {
    if let Some(i) = el.dyn_ref::<web_sys::HtmlInputElement>() {
        Some(i.value())
    } else if let Some(t) = el.dyn_ref::<web_sys::HtmlTextAreaElement>() {
        Some(t.value())
    } else if let Some(s) = el.dyn_ref::<web_sys::HtmlSelectElement>() {
        Some(s.value())
    } else {
        None
    }
}

fn find_item_ancestor(el: &Element, root: &Element) -> Option<String> {
    let mut cur: Option<Element> = Some(el.clone());
    while let Some(c) = cur {
        if let Some(json) = c.get_attribute("data-fx-item") {
            return Some(json);
        }
        if c == *root {
            return None;
        }
        cur = c.parent_element();
    }
    None
}

// ── network (MessagePack) ────────────────────────────────────────────────────

async fn fetch_snapshot(machine_id: &str) -> Result<Snapshot, JsValue> {
    let window = web_sys::window().unwrap();
    let resp: Response =
        JsFuture::from(window.fetch_with_str(&format!("/state?machine={machine_id}")))
            .await?
            .dyn_into()?;
    let bytes = response_bytes(resp).await?;
    rmp_serde::from_slice::<Snapshot>(&bytes)
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

async fn send_transition(
    machine_id: &str,
    event: &str,
    payload: Value,
) -> Result<Snapshot, JsValue> {
    let body_value = serde_json::json!({
        "machine": machine_id,
        "event": event,
        "payload": payload,
    });
    let body_bytes =
        rmp_serde::to_vec_named(&body_value).map_err(|e| JsValue::from_str(&e.to_string()))?;

    let uint8 = js_sys::Uint8Array::from(body_bytes.as_slice());
    let body_js: JsValue = uint8.into();

    let opts = RequestInit::new();
    opts.set_method("POST");
    opts.set_body(&body_js);
    opts.set_mode(RequestMode::SameOrigin);

    let req = Request::new_with_str_and_init("/transition", &opts)?;
    req.headers().set("content-type", "application/msgpack")?;

    let window = web_sys::window().unwrap();
    let resp: Response = JsFuture::from(window.fetch_with_request(&req))
        .await?
        .dyn_into()?;

    let bytes = response_bytes(resp).await?;
    rmp_serde::from_slice::<Snapshot>(&bytes)
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

async fn response_bytes(resp: Response) -> Result<Vec<u8>, JsValue> {
    let ab = JsFuture::from(resp.array_buffer()?).await?;
    Ok(js_sys::Uint8Array::new(&ab).to_vec())
}

// ── utilities ─────────────────────────────────────────────────────────────────

fn val_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn update_debug(document: &Document, snap: &Snapshot) {
    if let Some(el) = document.get_element_by_id("debug-snapshot") {
        el.set_text_content(Some(&serde_json::to_string_pretty(snap).unwrap_or_default()));
    }
}
