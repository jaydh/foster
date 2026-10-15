//! `fx-url="field->event"` on one machine root: the URL hash shows
//! `context[field]` (`#/photos/2024`), and the URL drives it back.
//!
//! - While the page is in sync, a snapshot whose field differs from the hash
//!   pushes a history entry, so Back/Forward move between values.
//! - Back/Forward (or editing the hash) fire `event` with `{field: value}`.
//! - When the page connects — its first snapshot, and every reconnect of
//!   the update stream — the URL wins: a link or bookmark opens that value,
//!   and a page whose session the server dropped (or whose server
//!   restarted) goes back to it instead of the instance's initial value.
//!
//! The value is kept as text with its `/`s, each segment URI-encoded.

use serde_json::{json, Value};
use std::cell::RefCell;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Document, Element};

struct Binding {
    root: Element,
    machine_id: String,
    session_id: String,
    cache_key: String,
    field: String,
    event: String,
    /// The first snapshot (or a reconnect's) has been checked against the URL.
    synced: bool,
    /// A value the URL asked for and the machine hasn't shown yet: don't
    /// push older snapshots' values over it meanwhile.
    pending: Option<String>,
}

thread_local! {
    static BINDING: RefCell<Option<Binding>> = const { RefCell::new(None) };
}

fn hash_value() -> Option<String> {
    let hash = web_sys::window()?.location().hash().ok()?;
    let raw = hash.strip_prefix('#').filter(|h| !h.is_empty())?;
    let parts: Option<Vec<String>> =
        raw.split('/').map(|seg| js_sys::decode_uri_component(seg).ok().and_then(|s| s.as_string())).collect();
    parts.map(|p| p.join("/"))
}

fn hash_for(value: &str) -> String {
    let segs: Vec<String> = value.split('/').map(|s| String::from(js_sys::encode_uri_component(s))).collect();
    format!("#{}", segs.join("/"))
}

fn set_hash(value: &str, push: bool) {
    let Some(window) = web_sys::window() else { return };
    let Ok(history) = window.history() else { return };
    let url = hash_for(value);
    let _ = if push {
        history.push_state_with_url(&JsValue::NULL, "", Some(&url))
    } else {
        history.replace_state_with_url(&JsValue::NULL, "", Some(&url))
    };
}

/// Register the page's `fx-url` root (the first one; there can be one).
pub fn bind(document: &Document, root: &Element, machine_id: &str, session_id: &str, cache_key: &str) {
    let Some(spec) = root.get_attribute("fx-url") else { return };
    let Some((field, event)) = spec.split_once("->") else { return };
    if BINDING.with(|b| b.borrow().is_some()) {
        web_sys::console::warn_1(&"foster: only the first fx-url on a page is used".into());
        return;
    }
    BINDING.with(|b| {
        *b.borrow_mut() = Some(Binding {
            root: root.clone(),
            machine_id: machine_id.into(),
            session_id: session_id.into(),
            cache_key: cache_key.into(),
            field: field.trim().into(),
            event: event.trim().into(),
            synced: false,
            pending: None,
        })
    });
    let doc = document.clone();
    let on_pop = Closure::<dyn FnMut()>::new(move || {
        let Some(want) = hash_value() else { return };
        let have = BINDING.with(|b| {
            b.borrow().as_ref().and_then(|b| crate::cached(&b.cache_key).and_then(|s| s.context.get(&b.field).and_then(Value::as_str).map(String::from)))
        });
        if have.as_deref() != Some(want.as_str()) {
            go(&doc, want);
        }
    });
    if let Some(w) = web_sys::window() {
        let _ = w.add_event_listener_with_callback("popstate", on_pop.as_ref().unchecked_ref());
    }
    on_pop.forget();
}

/// Fire the bound event for `value` (the URL asked for it).
fn go(document: &Document, value: String) {
    let Some((root, mid, sid, ck, field, event)) = BINDING.with(|b| {
        let mut b = b.borrow_mut();
        let b = b.as_mut()?;
        b.pending = Some(value.clone());
        Some((b.root.clone(), b.machine_id.clone(), b.session_id.clone(), b.cache_key.clone(), b.field.clone(), b.event.clone()))
    }) else {
        return;
    };
    let doc = document.clone();
    wasm_bindgen_futures::spawn_local(async move {
        match crate::dispatch(&mid, &event, json!({ field: value }), &sid, &ck).await {
            Ok(Some(snap)) => {
                crate::apply_snapshot_if_newer(&doc, &root, &snap);
            }
            Ok(None) => {}
            Err(e) => web_sys::console::error_1(&e),
        }
    });
}

/// A full snapshot of the bound machine arrived on (re)connecting: the URL
/// wins from here, if it names a value.
pub fn on_connect(root: &Element) {
    BINDING.with(|b| {
        if let Some(b) = b.borrow_mut().as_mut().filter(|b| b.root == *root) {
            b.synced = false;
        }
    });
}

/// After rendering `ctx` on `root`: bring the URL and the machine together.
pub fn follow(document: &Document, root: &Element, ctx: &Value) {
    enum Act {
        Nothing,
        Replace(String),
        Push(String),
        Go(String),
    }
    let act = BINDING.with(|b| {
        let mut b = b.borrow_mut();
        let Some(b) = b.as_mut().filter(|b| b.root == *root) else { return Act::Nothing };
        let Some(value) = ctx.get(&b.field).and_then(Value::as_str).map(String::from) else { return Act::Nothing };
        if !b.synced {
            b.synced = true;
            return match hash_value() {
                // Already asked for (the first snapshot, then the stream's).
                Some(want) if b.pending.as_deref() == Some(want.as_str()) => Act::Nothing,
                Some(want) if want != value => Act::Go(want),
                _ => Act::Replace(value),
            };
        }
        if let Some(p) = &b.pending {
            if *p != value {
                return Act::Nothing; // still on its way
            }
            b.pending = None;
        }
        if hash_value().as_deref() == Some(value.as_str()) { Act::Nothing } else { Act::Push(value) }
    });
    match act {
        Act::Nothing => {}
        Act::Replace(v) => set_hash(&v, false),
        Act::Push(v) => set_hash(&v, true),
        Act::Go(v) => go(document, v),
    }
}

// ── per-tab sessions ─────────────────────────────────────────────────────────

#[wasm_bindgen(inline_js = "
export function fx_tab_slot() {
  if (!navigator.locks) return Promise.resolve(-1);
  return (async () => {
    for (let i = 0; ; i++) {
      const held = await new Promise((resolve) =>
        navigator.locks.request('foster-tab-' + i, { ifAvailable: true }, (lock) => {
          resolve(!!lock);
          if (lock) return new Promise(() => {});
        }));
      if (held) return i;
    }
  })();
}")]
extern "C" {
    fn fx_tab_slot() -> js_sys::Promise;
}

/// This tab's slot: the lowest number no other open tab of this origin
/// holds (a Web Lock held until the tab closes), or None without Web Locks.
pub async fn tab_slot() -> Option<u32> {
    let n = wasm_bindgen_futures::JsFuture::from(fx_tab_slot()).await.ok()?.as_f64()?;
    (n >= 0.0).then_some(n as u32)
}
