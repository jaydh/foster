use foster_core::Snapshot;
use serde_json::Value;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{Request, RequestInit, RequestMode, Response};

// ── entry point ─────────────────────────────────────────────────────────────

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
        let node = roots.item(i).unwrap();
        let root: web_sys::Element = node.dyn_into().unwrap();
        let machine_id = root.get_attribute("fx-machine").unwrap();

        match fetch_snapshot(&machine_id).await {
            Ok(snap) => {
                apply_snapshot(&root, &snap);
                update_debug(&document, &snap);
                attach_listeners(document.clone(), root, machine_id);
            }
            Err(e) => web_sys::console::error_1(&e),
        }
    }
}

// ── DOM application ──────────────────────────────────────────────────────────

fn apply_snapshot(root: &web_sys::Element, snap: &Snapshot) {
    // Stamp data attributes for Playwright assertions:
    //   await expect(page.locator('[fx-machine="counter"]'))
    //         .toHaveAttribute('data-fx-state', 'idle');
    root.set_attribute("data-fx-state", &snap.state).unwrap();
    root.set_attribute("data-fx-version", &snap.version.to_string()).unwrap();

    apply_fx_show(root, &snap.state);
    apply_fx_text(root, &snap.context);
    apply_fx_disable(root, &snap.state);
    apply_fx_state_label(root, &snap.state);
}

/// `fx-show="idle,loading"` — element visible only in listed states.
fn apply_fx_show(root: &web_sys::Element, state: &str) {
    let els = root.query_selector_all("[fx-show]").unwrap();
    for i in 0..els.length() {
        let el: web_sys::HtmlElement = els.item(i).unwrap().dyn_into().unwrap();
        let attr = el.get_attribute("fx-show").unwrap_or_default();
        let visible = attr.split(',').any(|s| s.trim() == state);
        el.style()
            .set_property("display", if visible { "" } else { "none" })
            .unwrap();
    }
}

/// `fx-text="count"` — set text from a top-level context key.
fn apply_fx_text(root: &web_sys::Element, ctx: &Value) {
    let els = root.query_selector_all("[fx-text]").unwrap();
    for i in 0..els.length() {
        let el: web_sys::Element = els.item(i).unwrap().dyn_into().unwrap();
        let key = el.get_attribute("fx-text").unwrap_or_default();
        if let Some(val) = ctx.get(&key) {
            let text = match val {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            el.set_text_content(Some(&text));
        }
    }
}

/// `fx-disable="loading,saving"` — add `disabled` attribute in listed states.
fn apply_fx_disable(root: &web_sys::Element, state: &str) {
    let els = root.query_selector_all("[fx-disable]").unwrap();
    for i in 0..els.length() {
        let el: web_sys::Element = els.item(i).unwrap().dyn_into().unwrap();
        let attr = el.get_attribute("fx-disable").unwrap_or_default();
        if attr.split(',').any(|s| s.trim() == state) {
            el.set_attribute("disabled", "").unwrap();
        } else {
            el.remove_attribute("disabled").unwrap();
        }
    }
}

/// `fx-state-label` — display current state name (dev/debug).
fn apply_fx_state_label(root: &web_sys::Element, state: &str) {
    let els = root.query_selector_all("[fx-state-label]").unwrap();
    for i in 0..els.length() {
        let el: web_sys::Element = els.item(i).unwrap().dyn_into().unwrap();
        el.set_text_content(Some(state));
    }
}

// ── event wiring ─────────────────────────────────────────────────────────────

/// Attach listeners to all `[fx-on]` elements.  Called once on init.
/// Listeners survive snapshot re-application because we update the DOM in-place.
///
/// Attribute format: `fx-on="<dom-event>-><machine-event>"`
fn attach_listeners(document: web_sys::Document, root: web_sys::Element, machine_id: String) {
    let els = root.query_selector_all("[fx-on]").unwrap();
    for i in 0..els.length() {
        let el: web_sys::Element = els.item(i).unwrap().dyn_into().unwrap();
        let fx_on = el.get_attribute("fx-on").unwrap_or_default();
        let mut parts = fx_on.splitn(2, "->").map(|s| s.trim().to_string());
        let (Some(dom_event), Some(machine_event)) = (parts.next(), parts.next()) else {
            continue;
        };

        let mid = machine_id.clone();
        let root_clone = root.clone();
        let doc_clone = document.clone();

        let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| {
            let mid = mid.clone();
            let root = root_clone.clone();
            let doc = doc_clone.clone();
            let event = machine_event.clone();
            spawn_local(async move {
                match send_transition(&mid, &event, Value::Null).await {
                    Ok(snap) => {
                        apply_snapshot(&root, &snap);
                        update_debug(&doc, &snap);
                    }
                    Err(e) => web_sys::console::error_1(&e),
                }
            });
        });

        el.add_event_listener_with_callback(&dom_event, cb.as_ref().unchecked_ref())
            .unwrap();
        // Intentional leak: closures are page-lifetime singletons.
        cb.forget();
    }
}

// ── network (MessagePack) ────────────────────────────────────────────────────
//
// All state traffic between client and server is binary MessagePack.
// The test-injection endpoint (/test/state) stays JSON — it is intentionally
// curl-friendly and Playwright-friendly without a msgpack encoder.

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
    // serde_json::Value implements Serialize — pass it directly to rmp-serde.
    let body_value = serde_json::json!({
        "machine": machine_id,
        "event": event,
        "payload": payload,
    });
    let body_bytes =
        rmp_serde::to_vec_named(&body_value).map_err(|e| JsValue::from_str(&e.to_string()))?;

    // Wrap bytes in a Uint8Array for the Fetch body.
    let uint8 = js_sys::Uint8Array::from(body_bytes.as_slice());
    let body_js: JsValue = uint8.into();

    let opts = RequestInit::new();
    opts.set_method("POST");
    opts.set_body(&body_js);
    opts.set_mode(RequestMode::SameOrigin);

    let req_obj = Request::new_with_str_and_init("/transition", &opts)?;
    req_obj.headers().set("content-type", "application/msgpack")?;

    let window = web_sys::window().unwrap();
    let resp: Response = JsFuture::from(window.fetch_with_request(&req_obj))
        .await?
        .dyn_into()?;

    let bytes = response_bytes(resp).await?;
    rmp_serde::from_slice::<Snapshot>(&bytes)
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Read a `Response` body as raw bytes via `arrayBuffer()`.
async fn response_bytes(resp: Response) -> Result<Vec<u8>, JsValue> {
    let ab = JsFuture::from(resp.array_buffer()?).await?;
    let uint8 = js_sys::Uint8Array::new(&ab);
    Ok(uint8.to_vec())
}

// ── debug ─────────────────────────────────────────────────────────────────────

fn update_debug(document: &web_sys::Document, snap: &Snapshot) {
    if let Some(el) = document.get_element_by_id("debug-snapshot") {
        el.set_text_content(Some(&serde_json::to_string_pretty(snap).unwrap_or_default()));
    }
}
