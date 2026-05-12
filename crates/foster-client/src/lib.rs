use foster_core::Snapshot;
use serde_json::Value;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{Document, Element, EventSource, HtmlElement, MessageEvent, Request, RequestInit, RequestMode, Response};

// ── entry point ──────────────────────────────────────────────────────────────

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    spawn_local(bootstrap());
}

async fn bootstrap() {
    let window = web_sys::window().expect("no window");
    let document = window.document().expect("no document");

    let session_id = resolve_session_id(&window);

    let roots = document.query_selector_all("[fx-machine]").unwrap();
    for i in 0..roots.length() {
        let root: Element = roots.item(i).unwrap().dyn_into().unwrap();
        let machine_id = root.get_attribute("fx-machine").unwrap();

        // Stamp the session ID so Playwright can discover it.
        let _ = root.set_attribute("data-fx-session", &session_id);

        save_for_templates(&root);

        // Subscribe to SSE and attach the click listener *before* fetching
        // the initial snapshot.  This closes the race window where a Playwright
        // test-inject fires its SSE broadcast before the client is listening,
        // causing the inject to be silently lost.
        attach_sse_listener(document.clone(), root.clone(), machine_id.clone(), session_id.clone());
        attach_delegating_listener(document.clone(), root.clone(), machine_id.clone(), session_id.clone());

        match fetch_snapshot(&machine_id, &session_id).await {
            Ok(snap) => {
                // Use _if_newer so a concurrent inject (version ≥ 1 after the
                // restore() bump) is not clobbered by this v0 initial response.
                apply_snapshot_if_newer(&document, &root, &snap);
                update_debug(&document, &snap);
            }
            Err(e) => web_sys::console::error_1(&e),
        }
    }
}

// ── session ID ────────────────────────────────────────────────────────────────
//
// Reads `?session=<id>` from the URL; generates a random ID if absent.
// The session ID is stable for the lifetime of the page and is stamped onto
// `[fx-machine]` as `data-fx-session` so Playwright can pick it up and pass it
// to `POST /test/state?session=<id>` without a page.reload().

fn resolve_session_id(window: &web_sys::Window) -> String {
    if let Ok(search) = window.location().search() {
        for pair in search.trim_start_matches('?').split('&') {
            let mut parts = pair.splitn(2, '=');
            if parts.next() == Some("session") {
                if let Some(val) = parts.next() {
                    if !val.is_empty() {
                        return val.to_string();
                    }
                }
            }
        }
    }
    // Generate a 128-bit random ID from Math.random (good enough for tab isolation).
    let a = (js_sys::Math::random() * u32::MAX as f64) as u32;
    let b = (js_sys::Math::random() * u32::MAX as f64) as u32;
    let c = (js_sys::Math::random() * u32::MAX as f64) as u32;
    let d = (js_sys::Math::random() * u32::MAX as f64) as u32;
    format!("{a:08x}{b:08x}{c:08x}{d:08x}")
}

// ── snapshot application ─────────────────────────────────────────────────────

fn apply_snapshot(document: &Document, root: &Element, snap: &Snapshot) {
    root.set_attribute("data-fx-state",   &snap.state).unwrap();
    root.set_attribute("data-fx-version", &snap.version.to_string()).unwrap();

    apply_fx_show(root, &snap.state);
    apply_fx_text(root, &snap.context);
    apply_fx_disable(root, &snap.state);
    apply_fx_state_label(root, &snap.state);
    apply_fx_value(root, &snap.context);
    apply_fx_class(root, &snap.state);
    apply_fx_bind_attr(root, &snap.state, &snap.context);
    apply_fx_for(document, root, &snap.context);
}

/// Only apply if the incoming version is at least as new as what's displayed.
/// Guards against stale SSE pushes overwriting a more recent interactive transition.
fn apply_snapshot_if_newer(document: &Document, root: &Element, snap: &Snapshot) {
    let current: u64 = root.get_attribute("data-fx-version")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if snap.version >= current {
        apply_snapshot(document, root, snap);
        update_debug(document, snap);
    }
}

// ── attribute processors ─────────────────────────────────────────────────────

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

fn apply_fx_state_label(root: &Element, state: &str) {
    let els = root.query_selector_all("[fx-state-label]").unwrap();
    for i in 0..els.length() {
        let el: Element = els.item(i).unwrap().dyn_into().unwrap();
        el.set_text_content(Some(state));
    }
}

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

/// `fx-class="calm:gentle energized:vivid"` — toggle CSS classes based on state.
fn apply_fx_class(root: &Element, state: &str) {
    let els = root.query_selector_all("[fx-class]").unwrap();
    for i in 0..els.length() {
        let el: Element = els.item(i).unwrap().dyn_into().unwrap();
        let attr = el.get_attribute("fx-class").unwrap_or_default();
        let class_list = el.class_list();
        for pair in attr.split_whitespace() {
            let mut parts = pair.splitn(2, ':');
            if let (Some(st), Some(cls)) = (parts.next(), parts.next()) {
                if st == state {
                    let _ = class_list.add_1(cls);
                } else {
                    let _ = class_list.remove_1(cls);
                }
            }
        }
    }
}

/// `fx-bind-attr="href=ctx:url title=ctx:name disabled=state:loading"`
///
/// Space-separated pairs of `attr=source:value`:
///   `attr=ctx:key`        — set `attr` from `context[key]`
///   `attr=state:statename` — set `attr=""` when in that state, remove otherwise
fn apply_fx_bind_attr(root: &Element, state: &str, ctx: &Value) {
    let els = root.query_selector_all("[fx-bind-attr]").unwrap();
    for i in 0..els.length() {
        let el: Element = els.item(i).unwrap().dyn_into().unwrap();
        let attr_val = el.get_attribute("fx-bind-attr").unwrap_or_default();
        for pair in attr_val.split_whitespace() {
            let mut parts = pair.splitn(2, '=');
            let (Some(attr), Some(source)) = (parts.next(), parts.next()) else { continue };
            if let Some(key) = source.strip_prefix("ctx:") {
                match ctx.get(key) {
                    Some(val) => { let _ = el.set_attribute(attr, &val_to_string(val)); }
                    None      => { let _ = el.remove_attribute(attr); }
                }
            } else if let Some(target_state) = source.strip_prefix("state:") {
                if state == target_state {
                    let _ = el.set_attribute(attr, "");
                } else {
                    let _ = el.remove_attribute(attr);
                }
            }
        }
    }
}

fn apply_fx_for(document: &Document, root: &Element, ctx: &Value) {
    let containers = root.query_selector_all("[fx-for]").unwrap();
    for i in 0..containers.length() {
        let container: Element = containers.item(i).unwrap().dyn_into().unwrap();
        let key = container.get_attribute("fx-for").unwrap_or_default();

        let Some(full_array) = ctx.get(&key).and_then(|v| v.as_array()) else {
            continue;
        };

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
            continue;
        };

        container.set_inner_html("");

        for item in &array {
            let scratch = document.create_element("div").unwrap();
            scratch.set_inner_html(&template_html);
            let Some(item_el) = scratch.first_element_child() else { continue };

            item_el.set_attribute("data-fx-item", &item.to_string()).unwrap();

            if let Ok(el) = item_el.clone().dyn_into::<HtmlElement>() {
                let _ = el.style().remove_property("display");
            }

            apply_fx_fields(&item_el, item);
            container.append_child(&item_el).unwrap();
        }
    }
}

fn save_for_templates(root: &Element) {
    let containers = root.query_selector_all("[fx-for]").unwrap();
    for i in 0..containers.length() {
        let container: Element = containers.item(i).unwrap().dyn_into().unwrap();
        if container.get_attribute("data-fx-template-html").is_none() {
            let html = container.inner_html();
            container.set_attribute("data-fx-template-html", &html).unwrap();
        }
    }
}

fn apply_fx_fields(root: &Element, item: &Value) {
    if let Some(key) = root.get_attribute("fx-field") {
        if let Some(val) = item.get(&key) {
            root.set_text_content(Some(&val_to_string(val)));
        }
    }
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

fn attach_delegating_listener(document: Document, root: Element, machine_id: String, session_id: String) {
    let root_for_listener = root.clone();
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
        let root = &root_for_listener;
        let Some(target) = event.target() else { return };
        let Ok(target_el): Result<Element, _> = target.dyn_into() else { return };

        let Some(fx_on_el) = find_fx_on_ancestor(&target_el, root) else { return };

        let fx_on = fx_on_el.get_attribute("fx-on").unwrap_or_default();
        let mut parts = fx_on.splitn(2, "->").map(|s| s.trim().to_string());
        let (Some(dom_event), Some(machine_event)) = (parts.next(), parts.next()) else { return };

        if event.type_() != dom_event { return; }

        let payload = build_payload(&fx_on_el, root);
        let mid = machine_id.clone();
        let sid = session_id.clone();
        let root_clone = root.clone();
        let doc_clone = document.clone();

        spawn_local(async move {
            match send_transition(&mid, &machine_event, payload, &sid).await {
                Ok(snap) => {
                    apply_snapshot(&doc_clone, &root_clone, &snap);
                    update_debug(&doc_clone, &snap);
                }
                Err(e) => web_sys::console::error_1(&e),
            }
        });
    });

    root.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref()).unwrap();
    cb.forget();
}

// ── SSE listener ──────────────────────────────────────────────────────────────
//
// Opens a persistent Server-Sent Events connection to `/events?machine=...&session=...`.
// When the server broadcasts a new snapshot (e.g., after POST /test/state from Playwright),
// the client applies it immediately — no page.reload() needed.

fn attach_sse_listener(document: Document, root: Element, machine_id: String, session_id: String) {
    let url = format!("/events?machine={machine_id}&session={session_id}");
    let Ok(es) = EventSource::new(&url) else { return };

    let cb = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
        let data = match e.data().as_string() {
            Some(s) => s,
            None => return,
        };
        let snap: Snapshot = match serde_json::from_str(&data) {
            Ok(s) => s,
            Err(_) => return,
        };
        apply_snapshot_if_newer(&document, &root, &snap);
    });

    es.set_onmessage(Some(cb.as_ref().unchecked_ref()));
    cb.forget();
    // Prevent the EventSource from being dropped (closing the connection).
    std::mem::forget(es);
}

fn find_fx_on_ancestor(el: &Element, root: &Element) -> Option<Element> {
    let mut cur: Option<Element> = Some(el.clone());
    while let Some(c) = cur {
        if c.has_attribute("fx-on") { return Some(c); }
        if c == *root { return None; }
        cur = c.parent_element();
    }
    None
}

fn build_payload(fx_on_el: &Element, root: &Element) -> Value {
    let mut map = serde_json::Map::new();

    let els = root.query_selector_all("[fx-collect]").unwrap();
    for i in 0..els.length() {
        let el: Element = els.item(i).unwrap().dyn_into().unwrap();
        let key = el.get_attribute("fx-collect").unwrap_or_default();
        if let Some(val) = read_input_value(&el) {
            map.insert(key, Value::String(val));
        }
    }

    if let Some(item_json) = find_item_ancestor(fx_on_el, root) {
        if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&item_json) {
            for (k, v) in obj { map.entry(k).or_insert(v); }
        }
    }

    if let Some(payload_str) = fx_on_el.get_attribute("fx-payload") {
        if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&payload_str) {
            for (k, v) in obj { map.insert(k, v); }
        }
    }

    if map.is_empty() { Value::Null } else { Value::Object(map) }
}

fn read_input_value(el: &Element) -> Option<String> {
    if let Some(i) = el.dyn_ref::<web_sys::HtmlInputElement>()    { Some(i.value()) }
    else if let Some(t) = el.dyn_ref::<web_sys::HtmlTextAreaElement>() { Some(t.value()) }
    else if let Some(s) = el.dyn_ref::<web_sys::HtmlSelectElement>()   { Some(s.value()) }
    else { None }
}

fn find_item_ancestor(el: &Element, root: &Element) -> Option<String> {
    let mut cur: Option<Element> = Some(el.clone());
    while let Some(c) = cur {
        if let Some(json) = c.get_attribute("data-fx-item") { return Some(json); }
        if c == *root { return None; }
        cur = c.parent_element();
    }
    None
}

// ── network (MessagePack) ────────────────────────────────────────────────────

async fn fetch_snapshot(machine_id: &str, session_id: &str) -> Result<Snapshot, JsValue> {
    let window = web_sys::window().unwrap();
    let url = format!("/state?machine={machine_id}&session={session_id}");
    let resp: Response = JsFuture::from(window.fetch_with_str(&url)).await?.dyn_into()?;
    let bytes = response_bytes(resp).await?;
    rmp_serde::from_slice::<Snapshot>(&bytes).map_err(|e| JsValue::from_str(&e.to_string()))
}

async fn send_transition(
    machine_id: &str,
    event: &str,
    payload: Value,
    session_id: &str,
) -> Result<Snapshot, JsValue> {
    let body_value = serde_json::json!({
        "machine":  machine_id,
        "event":    event,
        "payload":  payload,
        "session":  session_id,
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
    let resp: Response = JsFuture::from(window.fetch_with_request(&req)).await?.dyn_into()?;
    let bytes = response_bytes(resp).await?;
    rmp_serde::from_slice::<Snapshot>(&bytes).map_err(|e| JsValue::from_str(&e.to_string()))
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
