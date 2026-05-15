use foster_core::Snapshot;
use serde::Deserialize;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{Document, Element, EventSource, HtmlElement, MessageEvent, Request, RequestInit, RequestMode, Response};

// ── context cache ─────────────────────────────────────────────────────────────
//
// Tracks the last context Value received over SSE per machine instance, keyed
// by `"{machine_id}:{effective_session}"`.  This makes the key unique across
// both different machine types and multiple instances of the same type on the
// same page (achieved via the `fx-machine="counter#1"` fragment syntax).
// Used to apply JSON Patch diffs from "patch" SSE events against the correct
// base.  Only SSE events (snapshot/patch) update this cache — direct transition
// responses do not, keeping the cache aligned with the server's SSE diff chain.

thread_local! {
    static CONTEXT_CACHE: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

fn store_context(cache_key: &str, ctx: Value) {
    CONTEXT_CACHE.with(|c| { c.borrow_mut().insert(cache_key.to_string(), ctx); });
}

fn load_context(cache_key: &str) -> Value {
    CONTEXT_CACHE.with(|c| {
        c.borrow().get(cache_key).cloned().unwrap_or_else(|| Value::Object(Default::default()))
    })
}

/// Deserialized form of the server's `ContextPatch` SSE event.
#[derive(Deserialize)]
struct ContextPatch {
    machine_id: String,
    state: String,
    version: u64,
    #[serde(default)]
    last_event: Option<String>,
    patch: json_patch::Patch,
}

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
        let raw_attr = root.get_attribute("fx-machine").unwrap();

        // Split `"counter#1"` → machine_id="counter", fragment="1".
        // The fragment is appended to the session ID so each instance on
        // the page talks to a distinct server-side (session, machine) pair.
        let (machine_id, fragment) = split_instance(&raw_attr);
        let effective_session = if fragment.is_empty() {
            session_id.clone()
        } else {
            format!("{session_id}.{fragment}")
        };

        // Cache key unique per (machine type, instance) on this page.
        let cache_key = format!("{machine_id}:{effective_session}");

        // Stamp the effective session so Playwright can discover it.
        let _ = root.set_attribute("data-fx-session", &effective_session);

        save_for_templates(&root);

        // Subscribe to SSE and attach the click listener *before* fetching
        // the initial snapshot.  This closes the race window where a Playwright
        // test-inject fires its SSE broadcast before the client is listening,
        // causing the inject to be silently lost.
        attach_sse_listener(document.clone(), root.clone(), machine_id.clone(), effective_session.clone(), cache_key.clone());
        attach_delegating_listener(document.clone(), root.clone(), machine_id.clone(), effective_session.clone());

        #[cfg(debug_assertions)]
        mount_overlay(&document, &root, &machine_id, &effective_session);

        match fetch_snapshot(&machine_id, &effective_session).await {
            Ok(snap) => {
                // Seed the context cache so the first SSE "patch" event has a valid base.
                store_context(&cache_key, snap.context.clone());
                // Use _if_newer so a concurrent inject (version ≥ 1 after the
                // restore() bump) is not clobbered by this v0 initial response.
                apply_snapshot_if_newer(&document, &root, &snap);
                update_debug(&document, &root, &snap);
            }
            Err(e) => web_sys::console::error_1(&e),
        }
    }
}

// ── instance addressing ───────────────────────────────────────────────────────
//
// `fx-machine="counter#1"` → machine_id="counter", fragment="1"
// `fx-machine="counter"`   → machine_id="counter", fragment=""
//
// The fragment is appended to the page's base session ID (with a `.` separator)
// to form the effective session sent to the server.  This lets two `counter`
// roots on the same page address independent server-side instances:
//
//   fx-machine="counter#a" → POST /transition?session=<base>.a  machine=counter
//   fx-machine="counter#b" → POST /transition?session=<base>.b  machine=counter

fn split_instance(raw: &str) -> (String, String) {
    match raw.find('#') {
        Some(i) => (raw[..i].to_string(), raw[i + 1..].to_string()),
        None => (raw.to_string(), String::new()),
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
        update_debug(document, root, snap);
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
            .set_property("display", if visible { "block" } else { "none" })
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
            } else if let Some(sel) = el.dyn_ref::<web_sys::HtmlSelectElement>() {
                sel.set_value(&text);
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
                    update_debug(&doc_clone, &root_clone, &snap);
                }
                Err(e) => web_sys::console::error_1(&e),
            }
        });
    });

    // Register for click, change (select/input blur), and input (live text) so
    // fx-on="change->..." and fx-on="input->..." work alongside fx-on="click->...".
    for ev_type in &["click", "change", "input"] {
        root.add_event_listener_with_callback(ev_type, cb.as_ref().unchecked_ref()).unwrap();
    }
    cb.forget();
}

// ── SSE listener ──────────────────────────────────────────────────────────────
//
// Opens a persistent Server-Sent Events connection to `/events?machine=...&session=...`.
// When the server broadcasts a new snapshot (e.g., after POST /test/state from Playwright),
// the client applies it immediately — no page.reload() needed.

fn attach_sse_listener(document: Document, root: Element, machine_id: String, session_id: String, cache_key: String) {
    let url = format!("/events?machine={machine_id}&session={session_id}");
    let Ok(es) = EventSource::new(&url) else { return };

    // "snapshot" — full state; first event on each SSE connection (or reconnect).
    // Always update the context cache so subsequent patches have a valid base,
    // even if the DOM version check skips the render.
    {
        let document = document.clone();
        let root = root.clone();
        let cache_key = cache_key.clone();
        let cb = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let data = match e.data().as_string() { Some(s) => s, None => return };
            let snap: Snapshot = match serde_json::from_str(&data) { Ok(s) => s, Err(_) => return };
            store_context(&cache_key, snap.context.clone());
            apply_snapshot_if_newer(&document, &root, &snap);
        });
        es.add_event_listener_with_callback("snapshot", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

    // "patch" — RFC 6902 JSON Patch of the context field only.
    // Always apply the patch to the cache (keeps the diff chain intact even when
    // the DOM is already at a newer version from a direct transition response).
    {
        let cache_key = cache_key.clone();
        let cb = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let data = match e.data().as_string() { Some(s) => s, None => return };
            let pe: ContextPatch = match serde_json::from_str(&data) { Ok(p) => p, Err(_) => return };
            let mut ctx = load_context(&cache_key);
            if json_patch::patch(&mut ctx, &pe.patch.0).is_err() { return; }
            store_context(&cache_key, ctx.clone());
            let snap = Snapshot {
                machine_id: pe.machine_id,
                state: pe.state,
                version: pe.version,
                last_event: pe.last_event,
                context: ctx,
            };
            apply_snapshot_if_newer(&document, &root, &snap);
        });
        es.add_event_listener_with_callback("patch", cb.as_ref().unchecked_ref()).unwrap();
        cb.forget();
    }

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

fn update_debug(document: &Document, root: &Element, snap: &Snapshot) {
    #[cfg(debug_assertions)]
    update_overlay(document, root, snap);
    #[cfg(not(debug_assertions))]
    let _ = (document, root, snap);
}

// ── dev overlay (debug builds only) ──────────────────────────────────────────
// CSS is served by foster-server (in the HTML before WASM runs) so there's no
// render-cycle flash. The WASM creates the panel DOM and wires all event handlers.

#[cfg(debug_assertions)]
fn read_machine_states(machine_id: &str) -> Vec<String> {
    let global = js_sys::global();
    let Ok(machines) = js_sys::Reflect::get(&global, &JsValue::from_str("__FOSTER_MACHINES"))
    else {
        return vec![];
    };
    if machines.is_undefined() || machines.is_null() {
        return vec![];
    }
    let Ok(states) = js_sys::Reflect::get(&machines, &JsValue::from_str(machine_id)) else {
        return vec![];
    };
    if !js_sys::Array::is_array(&states) {
        return vec![];
    }
    js_sys::Array::from(&states)
        .iter()
        .filter_map(|v| v.as_string())
        .collect()
}

/// Mount the debug overlay panel for one `[fx-machine]` root.
///
/// The panel ID incorporates both the machine name and the effective session so
/// that two instances of the same machine on the same page (`fx-machine="counter#1"`
/// and `fx-machine="counter#2"`) each get their own independent panel.
/// The panel ID is also stamped as `data-fx-panel` on the root so that
/// `update_overlay` can find the right panel when a snapshot arrives.
#[cfg(debug_assertions)]
fn mount_overlay(document: &Document, root: &Element, machine_id: &str, session_id: &str) {
    // Replace '.' (fragment separator) with '-' for a CSS-safe element ID.
    let safe_session = session_id.replace('.', "-");
    let panel_id = format!("fx-dbg-{machine_id}-{safe_session}");

    if document.get_element_by_id(&panel_id).is_some() {
        return;
    }

    // Stamp so update_overlay can route snapshot updates to the right panel.
    let _ = root.set_attribute("data-fx-panel", &panel_id);

    let states = read_machine_states(machine_id);
    let short_sid = if session_id.len() > 8 { &session_id[..8] } else { session_id };
    let options_html: String = states.iter()
        .map(|s| format!("<option>{s}</option>"))
        .collect();

    let enc_machine = percent_encode(machine_id);
    let enc_session = percent_encode(session_id);

    let panel = document.create_element("div").unwrap();
    panel.set_id(&panel_id);
    panel.set_attribute("class", "fx-dbg").unwrap();
    panel.set_inner_html(&format!(
        "<div class=\"fx-dbg-head\">\
          <span style=\"color:#4a9eff;font-weight:bold\">Foster</span>\
          <span style=\"flex:1;color:#555;white-space:nowrap;overflow:hidden;text-overflow:ellipsis\" title=\"{session_id}\"> {short_sid}\u{2026}</span>\
          <button class=\"fx-dbg-ctrl fx-dbg-mn\">\u{2014}</button>\
          <button class=\"fx-dbg-ctrl fx-dbg-cl\">\u{d7}</button>\
        </div>\
        <div class=\"fx-dbg-body\">\
          <div class=\"fx-dbg-row\"><span class=\"fx-dbg-key\">machine</span><span>{machine_id}</span></div>\
          <div class=\"fx-dbg-row\"><span class=\"fx-dbg-key\">state</span><span class=\"fx-dbg-st\">\u{2014}</span></div>\
          <div class=\"fx-dbg-row\"><span class=\"fx-dbg-key\">version</span><span class=\"fx-dbg-ver\">\u{2014}</span></div>\
          <div class=\"fx-dbg-row\"><span class=\"fx-dbg-key\">event</span><span class=\"fx-dbg-ev\" style=\"color:#aaa\">\u{2014}</span></div>\
          <div class=\"fx-dbg-jump\">\
            <select class=\"fx-dbg-sel\">{options_html}</select>\
            <button class=\"fx-dbg-go\">\u{2192}</button>\
          </div>\
          <div class=\"fx-dbg-links\">\
            <a href=\"/debug/graph?machine={enc_machine}&session={enc_session}\" target=\"_blank\">graph \u{2197}</a>\
            <a href=\"/debug/timeline?machine={enc_machine}&session={enc_session}\" target=\"_blank\">timeline \u{2197}</a>\
          </div>\
        </div>"
    ));

    let minimized = web_sys::window()
        .and_then(|w| w.session_storage().ok().flatten())
        .and_then(|s| s.get_item("fx-dbg-min").ok().flatten())
        .map(|v| v == "1")
        .unwrap_or(false);
    if minimized {
        panel.class_list().add_1("min").unwrap();
        if let Some(btn) = panel.query_selector(".fx-dbg-mn").ok().flatten() {
            btn.set_text_content(Some("\u{25a1}"));
        }
    }

    // Minimize
    let panel_mn = panel.clone();
    let mn_cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| {
        let minimized = panel_mn.class_list().toggle("min").unwrap_or(false);
        if let Some(btn) = panel_mn.query_selector(".fx-dbg-mn").ok().flatten() {
            btn.set_text_content(Some(if minimized { "\u{25a1}" } else { "\u{2014}" }));
        }
        if let Some(s) = web_sys::window().and_then(|w| w.session_storage().ok().flatten()) {
            let _ = s.set_item("fx-dbg-min", if minimized { "1" } else { "0" });
        }
    });
    if let Some(btn) = panel.query_selector(".fx-dbg-mn").ok().flatten() {
        btn.add_event_listener_with_callback("click", mn_cb.as_ref().unchecked_ref()).unwrap();
    }
    mn_cb.forget();

    // Close
    let panel_cl = panel.clone();
    let cl_cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| {
        panel_cl.remove();
    });
    if let Some(btn) = panel.query_selector(".fx-dbg-cl").ok().flatten() {
        btn.add_event_listener_with_callback("click", cl_cb.as_ref().unchecked_ref()).unwrap();
    }
    cl_cb.forget();

    // Jump to state
    let panel_go = panel.clone();
    let mid_go = machine_id.to_string();
    let sid_go = session_id.to_string();
    let go_cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| {
        let target_state = panel_go
            .query_selector(".fx-dbg-sel").ok().flatten()
            .and_then(|el| el.dyn_into::<web_sys::HtmlSelectElement>().ok())
            .map(|s| s.value())
            .unwrap_or_default();
        if target_state.is_empty() { return; }

        let last_ctx: Value = panel_go
            .get_attribute("data-fx-last-ctx")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Object(Default::default()));

        let body = serde_json::json!({
            "machine_id": mid_go,
            "state": target_state,
            "context": last_ctx,
            "version": 0,
        });
        let url = format!("/test/state?session={}", sid_go);
        let body_str = body.to_string();
        spawn_local(async move {
            let opts = RequestInit::new();
            opts.set_method("POST");
            opts.set_body(&JsValue::from_str(&body_str));
            opts.set_mode(RequestMode::SameOrigin);
            let Ok(req) = Request::new_with_str_and_init(&url, &opts) else { return };
            let _ = req.headers().set("content-type", "application/json");
            let window = web_sys::window().unwrap();
            let _ = JsFuture::from(window.fetch_with_request(&req)).await;
        });
    });
    if let Some(btn) = panel.query_selector(".fx-dbg-go").ok().flatten() {
        btn.add_event_listener_with_callback("click", go_cb.as_ref().unchecked_ref()).unwrap();
    }
    go_cb.forget();

    if let Some(body) = document.query_selector("body").ok().flatten() {
        body.append_child(&panel).unwrap();
    }
}

#[cfg(debug_assertions)]
fn update_overlay(document: &Document, root: &Element, snap: &Snapshot) {
    let panel_id = root.get_attribute("data-fx-panel").unwrap_or_default();
    let Some(panel) = document.get_element_by_id(&panel_id) else { return };

    if let Some(el) = panel.query_selector(".fx-dbg-st").ok().flatten() {
        el.set_text_content(Some(&snap.state));
    }
    if let Some(el) = panel.query_selector(".fx-dbg-ver").ok().flatten() {
        el.set_text_content(Some(&format!("v{}", snap.version)));
    }
    if let Some(el) = panel.query_selector(".fx-dbg-ev").ok().flatten() {
        el.set_text_content(Some(snap.last_event.as_deref().unwrap_or("\u{2014}")));
    }
    let _ = panel.set_attribute("data-fx-last-ctx", &snap.context.to_string());
}

#[cfg(debug_assertions)]
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::split_instance;

    #[test]
    fn split_no_fragment() {
        let (machine, fragment) = split_instance("counter");
        assert_eq!(machine, "counter");
        assert_eq!(fragment, "");
    }

    #[test]
    fn split_with_fragment() {
        let (machine, fragment) = split_instance("counter#1");
        assert_eq!(machine, "counter");
        assert_eq!(fragment, "1");
    }

    #[test]
    fn split_with_alphanumeric_fragment() {
        let (machine, fragment) = split_instance("player#main");
        assert_eq!(machine, "player");
        assert_eq!(fragment, "main");
    }

    #[test]
    fn split_only_first_hash() {
        // Fragment containing '#' — only the first hash is the separator.
        let (machine, fragment) = split_instance("counter#a#b");
        assert_eq!(machine, "counter");
        assert_eq!(fragment, "a#b");
    }

    #[test]
    fn split_empty_fragment() {
        // Trailing '#' with no fragment text.
        let (machine, fragment) = split_instance("counter#");
        assert_eq!(machine, "counter");
        assert_eq!(fragment, "");
    }
}
