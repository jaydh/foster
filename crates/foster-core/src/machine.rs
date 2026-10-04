use crate::snapshot::Snapshot;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum MachineError {
    #[error("unknown state: '{0}'")]
    UnknownState(String),
    #[error("no transition '{event}' from state '{state}'")]
    InvalidTransition { state: String, event: String },
    #[error("reducer failed: {0}")]
    ReducerError(String),
    #[error("context schema violation in state '{state}': {message}")]
    SchemaViolation { state: String, message: String },
    #[error("context parse error: {0}")]
    ContextParse(String),
}

type ReducerFn = Arc<dyn Fn(Value, Value) -> Result<Value, MachineError> + Send + Sync>;

/// A single edge in the state graph.
pub struct TransitionDef {
    /// Target state name after this transition fires.
    pub target: String,
    /// Reducer: `(old_context, event_payload) → new_context`.
    /// `None` passes context through unchanged.
    pub reduce: Option<ReducerFn>,
    /// Declarative form of `reduce`, when there is one (`.pass()` / `.merge()`).
    /// `None` for arbitrary Rust reducers, which can only run on the server —
    /// so a `.local()` machine must have `Some` on every edge.
    pub local: Option<LocalReduce>,
}

/// A reducer simple enough to describe as data, so the generic WASM client can
/// run it for `.local()` machines without any app-specific Rust.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalReduce {
    /// Context unchanged.
    Pass,
    /// Shallow-merge the event payload's top-level keys into the context.
    Merge,
    /// List navigation — see [`step_list`].
    Step { list: String, index: String, by: i64 },
}

impl LocalReduce {
    pub fn apply(&self, ctx: Value, payload: Value) -> Value {
        match self {
            LocalReduce::Pass => ctx,
            LocalReduce::Merge => merge_shallow(ctx, payload),
            LocalReduce::Step { list, index, by } => step_list(ctx, list, index, *by),
        }
    }
}

/// Move `ctx[index]` by `by` through the array `ctx[list]`, wrapping at both
/// ends, then shallow-merge the selected item's fields into the context — so
/// markup binds the current item as plain `ctx:` keys (a lightbox's
/// `src=ctx:url`, a wizard's `fx-text="title"`). A missing/non-numeric index
/// counts as 0; an empty or missing list leaves the context unchanged.
pub fn step_list(ctx: Value, list: &str, index: &str, by: i64) -> Value {
    let len = ctx.get(list).and_then(Value::as_array).map_or(0, Vec::len) as i64;
    if len == 0 {
        return ctx;
    }
    let current = ctx.get(index).and_then(Value::as_i64).unwrap_or(0);
    let next = (current + by).rem_euclid(len);
    let item = ctx[list][next as usize].clone();
    let mut out = merge_shallow(ctx, item);
    out[index] = Value::from(next);
    out
}

/// `ctx` with `payload`'s top-level keys written over it. A non-object payload
/// (including `null`, what the client sends for an event with no payload)
/// leaves `ctx` unchanged; a non-object `ctx` is replaced by an empty object.
pub fn merge_shallow(ctx: Value, payload: Value) -> Value {
    let Value::Object(add) = payload else { return ctx };
    let mut base = match ctx {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    base.extend(add);
    Value::Object(base)
}

/// Wire form of a `.local()` machine, embedded in the served page as JSON so
/// the WASM client can run it entirely in the browser.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalMachineDef {
    pub id: String,
    pub initial_state: String,
    pub initial_context: Value,
    /// Save state + context to `localStorage` and restore it on the next load.
    pub persist: bool,
    /// state → event → edge
    pub transitions: HashMap<String, HashMap<String, LocalTransition>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalTransition {
    pub target: String,
    pub reduce: LocalReduce,
}

impl LocalMachineDef {
    /// Whether `state` is a state of this machine (has an entry, even with no outgoing edges).
    pub fn has_state(&self, state: &str) -> bool {
        self.transitions.contains_key(state)
    }

    /// Same semantics as `MachineInstance::send` for one edge, minus versioning
    /// (the caller owns that). Returns `(next_state, next_context)`.
    pub fn send(
        &self,
        state: &str,
        ctx: Value,
        event: &str,
        payload: Value,
    ) -> Result<(String, Value), MachineError> {
        let edges = self
            .transitions
            .get(state)
            .ok_or_else(|| MachineError::UnknownState(state.to_string()))?;
        let edge = edges.get(event).ok_or_else(|| MachineError::InvalidTransition {
            state: state.to_string(),
            event: event.to_string(),
        })?;
        Ok((edge.target.clone(), edge.reduce.apply(ctx, payload)))
    }
}

/// The static, shared definition of a machine.  Construct via `MachineBuilder`.
/// `Arc<Machine>` is cheaply cloneable and can be shared across threads.
pub struct Machine {
    pub id: String,
    pub initial_state: String,
    pub initial_context: Value,
    /// state_name → event_name → TransitionDef
    pub(crate) states: HashMap<String, HashMap<String, TransitionDef>>,
    /// Optional JSON Schema per state, validated on every state entry.
    pub(crate) state_schemas: HashMap<String, Value>,
    /// Optional HTML template.  When present, `foster_server::router` serves it at `GET /`
    /// and validates all `fx-show` / `fx-on` attributes at startup.
    pub template: Option<String>,
    /// Runs in the browser (per visitor) instead of on the server. See `MachineBuilder::local`.
    pub local: bool,
    /// Local machine whose state survives reloads via `localStorage`.
    pub persist: bool,
    /// One server-side instance for every visitor (the session is ignored). See `MachineBuilder::shared`.
    pub shared: bool,
}

impl Machine {
    /// The client-side definition of a `.local()` machine; `None` for server machines.
    pub fn local_def(&self) -> Option<LocalMachineDef> {
        if !self.local {
            return None;
        }
        let transitions = self
            .states
            .iter()
            .map(|(from, events)| {
                let edges = events
                    .iter()
                    .map(|(event, def)| {
                        let reduce = def.local.clone().expect("checked in MachineBuilder::build");
                        (event.clone(), LocalTransition { target: def.target.clone(), reduce })
                    })
                    .collect();
                (from.clone(), edges)
            })
            .collect();
        Some(LocalMachineDef {
            id: self.id.clone(),
            initial_state: self.initial_state.clone(),
            initial_context: self.initial_context.clone(),
            persist: self.persist,
            transitions,
        })
    }

    /// All valid state names in definition order.
    pub fn state_names(&self) -> Vec<&str> {
        self.states.keys().map(|s| s.as_str()).collect()
    }

    /// All (from_state, event, to_state) triples — used by test generation.
    pub fn transitions(&self) -> Vec<(&str, &str, &str)> {
        self.states
            .iter()
            .flat_map(|(from, events)| {
                events
                    .iter()
                    .map(move |(event, def)| (from.as_str(), event.as_str(), def.target.as_str()))
            })
            .collect()
    }

    /// Validate `fx-show` and `fx-on` attribute values in the template against the machine.
    ///
    /// Returns `Ok(())` when there is no template or all references are valid.
    /// Returns `Err(errors)` listing every unknown state or event name found.
    ///
    /// Called automatically by `foster_server::router` at startup — a misconfigured template
    /// panics the server immediately rather than silently misbehaving at runtime.
    ///
    /// Only validates within this machine's own `[fx-machine="{id}"]` subtree(s) — see
    /// [`Self::validate_in`] for the multi-machine-per-page case.
    pub fn validate_template(&self) -> Result<(), Vec<String>> {
        match &self.template {
            Some(html) => self.validate_in(html),
            None => Ok(()),
        }
    }

    /// Like [`Self::validate_template`], but takes an externally-supplied page HTML
    /// instead of `self.template`. Used when several distinct machines share one
    /// page: each machine only owns the `[fx-machine="{id}"]` / `[fx-machine="{id}#..."]`
    /// subtree(s) matching its own id, so validation is scoped to those subtrees
    /// rather than the whole document (which may contain other machines' vocabulary).
    pub fn validate_in(&self, html: &str) -> Result<(), Vec<String>> {
        let valid_states: std::collections::HashSet<&str> =
            self.states.keys().map(|s| s.as_str()).collect();
        let valid_events: std::collections::HashSet<&str> = self
            .states
            .values()
            .flat_map(|events| events.keys().map(|e| e.as_str()))
            .collect();

        let subtrees = extract_machine_subtrees(html, &self.id);
        // No `[fx-machine="{id}"]` wrapper found at all: either this fragment predates
        // the wrapper convention (tests often pass bare fragments), or it belongs to a
        // different machine sharing this page — validate the whole thing either way,
        // since a false negative (silently skipping validation) is worse than a false
        // positive on a machine that turns out to have no markup on this page.
        let owned_subtrees: Vec<String>;
        let scopes: &[String] = if subtrees.is_empty() {
            owned_subtrees = vec![html.to_string()];
            &owned_subtrees
        } else {
            &subtrees
        };

        let mut errors = Vec::new();

        for subtree in scopes {
            // A machine's subtree may itself contain a *nested* `[fx-machine]` of a
            // different id — e.g. a page-wide "theme" wrapper containing a "nav"
            // machine for a dropdown. That nested region belongs to the other
            // machine, not this one, so carve it out before scanning for our own
            // fx-show/fx-on references.
            let subtree = strip_nested_machines(subtree);
            let subtree = subtree.as_str();

            for val in extract_attr_values(subtree, "fx-show") {
                for state in val.split(',') {
                    let state = state.trim();
                    if !state.is_empty() && !valid_states.contains(state) {
                        errors.push(format!(
                            "fx-show=\"{val}\": state '{state}' not defined in machine '{}'",
                            self.id
                        ));
                    }
                }
            }

            for val in extract_attr_values(subtree, "fx-on") {
                if let Some(event) = val.splitn(2, "->").nth(1) {
                    let event = event.trim();
                    if !event.is_empty() && !valid_events.contains(event) {
                        errors.push(format!(
                            "fx-on=\"{val}\": event '{event}' not defined in machine '{}'",
                            self.id
                        ));
                    }
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// Extract all values of a named attribute from an HTML string.
/// Handles `attr="value"` (double-quoted).  Fast string scan, no parser dependency.
fn extract_attr_values(html: &str, attr: &str) -> Vec<String> {
    let needle = format!("{attr}=\"");
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find(needle.as_str()) {
        rest = &rest[i + needle.len()..];
        if let Some(j) = rest.find('"') {
            out.push(rest[..j].to_string());
            rest = &rest[j + 1..];
        } else {
            break;
        }
    }
    out
}

/// Find the element(s) with `fx-machine="{machine_id}"` or `fx-machine="{machine_id}#..."`
/// (the `#instance` fragment used for multiple instances of the same machine on one
/// page) and return each one's full outer HTML, tag depth-matched to its closing tag.
///
/// Fast string scan in the same spirit as `extract_attr_values` — not a real HTML
/// parser, so it assumes well-formed, non-self-closing markup for the matched tag.
fn extract_machine_subtrees(html: &str, machine_id: &str) -> Vec<String> {
    let needle = format!("fx-machine=\"{machine_id}");
    let mut out = Vec::new();
    let mut search_from = 0;

    while let Some(rel) = html[search_from..].find(needle.as_str()) {
        let attr_pos = search_from + rel;
        let after = &html[attr_pos + needle.len()..];
        // Must be an exact id match: value ends here (`"`) or continues as `#instance` (`#`).
        // Otherwise this is a different id with the same prefix (e.g. "counter" vs "counter2").
        if !(after.starts_with('"') || after.starts_with('#')) {
            search_from = attr_pos + needle.len();
            continue;
        }

        let Some(tag_start) = html[..attr_pos].rfind('<') else {
            search_from = attr_pos + needle.len();
            continue;
        };
        let tag_name_start = tag_start + 1;
        let tag_name_end = html[tag_name_start..]
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .map(|o| tag_name_start + o)
            .unwrap_or(html.len());
        let tag_name = &html[tag_name_start..tag_name_end];

        let Some(open_tag_end_rel) = html[tag_start..].find('>') else {
            search_from = attr_pos + needle.len();
            continue;
        };
        let content_start = tag_start + open_tag_end_rel + 1;

        match find_matching_close(html, tag_name, content_start) {
            Some(end) => {
                out.push(html[tag_start..end].to_string());
                search_from = end;
            }
            None => {
                // Malformed / no closing tag — take the rest of the document rather
                // than silently dropping this machine's validation.
                out.push(html[tag_start..].to_string());
                break;
            }
        }
    }

    out
}

/// Remove any nested `[fx-machine="..."]` subtree(s) from `subtree` (which itself starts
/// at some outer `<tag fx-machine="...">` and ends at its matching close tag). Skips past
/// the outer tag's own opening `>` first, so the outer wrapper's own `fx-machine` attribute
/// is never mistaken for a nested one.
fn strip_nested_machines(subtree: &str) -> String {
    let Some(first_gt) = subtree.find('>') else {
        return subtree.to_string();
    };
    let (head, body) = subtree.split_at(first_gt + 1);

    let mut result = String::from(head);
    let mut rest = body;
    loop {
        let Some(rel) = rest.find("fx-machine=\"") else {
            result.push_str(rest);
            break;
        };
        let Some(tag_start) = rest[..rel].rfind('<') else {
            result.push_str(rest);
            break;
        };
        result.push_str(&rest[..tag_start]);

        let tag_name_start = tag_start + 1;
        let tag_name_end = rest[tag_name_start..]
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .map(|o| tag_name_start + o)
            .unwrap_or(rest.len());
        let tag_name = &rest[tag_name_start..tag_name_end].to_string();

        let Some(open_end_rel) = rest[tag_start..].find('>') else {
            result.push_str(&rest[tag_start..]);
            break;
        };
        let content_start = tag_start + open_end_rel + 1;

        match find_matching_close(rest, tag_name, content_start) {
            Some(end) => rest = &rest[end..],
            None => break,
        }
    }
    result
}

/// From `from` (just after an opening `<tag ...>`), depth-count nested `<tag` / `</tag>`
/// occurrences of the same tag name and return the index just past the matching `</tag>`.
fn find_matching_close(html: &str, tag: &str, from: usize) -> Option<usize> {
    let mut depth = 1u32;
    let mut pos = from;
    loop {
        let next_open = find_tag_boundary(html, tag, pos, false);
        let next_close = find_tag_boundary(html, tag, pos, true);
        match (next_open, next_close) {
            (_, None) => return None,
            (Some(o), Some(c)) if o < c => {
                depth += 1;
                pos = o + 1 + tag.len();
            }
            (_, Some(c)) => {
                depth -= 1;
                let close_end = html[c..].find('>').map(|o| c + o + 1)?;
                if depth == 0 {
                    return Some(close_end);
                }
                pos = close_end;
            }
        }
    }
}

/// Find the next `<tag` (or `</tag` when `closing`) at a proper tag-name boundary
/// (followed by whitespace, `>`, or `/`) starting at or after `from`.
fn find_tag_boundary(html: &str, tag: &str, from: usize, closing: bool) -> Option<usize> {
    let needle = if closing { format!("</{tag}") } else { format!("<{tag}") };
    let mut search_from = from;
    loop {
        let rel = html[search_from..].find(needle.as_str())?;
        let idx = search_from + rel;
        let after = idx + needle.len();
        let boundary = html[after..]
            .chars()
            .next()
            .map(|c| c.is_whitespace() || c == '>' || c == '/')
            .unwrap_or(true);
        if boundary {
            return Some(idx);
        }
        search_from = idx + needle.len();
    }
}

/// Builder for `Machine`.  All methods consume and return `Self` for chaining.
pub struct MachineBuilder {
    id: String,
    initial_state: String,
    initial_context: Value,
    states: HashMap<String, HashMap<String, TransitionDef>>,
    state_schemas: HashMap<String, Value>,
    template: Option<String>,
    local: bool,
    persist: bool,
    shared: bool,
}

impl MachineBuilder {
    pub fn new(
        id: impl Into<String>,
        initial_state: impl Into<String>,
        initial_context: Value,
    ) -> Self {
        let initial_state = initial_state.into();
        let mut states: HashMap<String, HashMap<String, TransitionDef>> = HashMap::new();
        states.entry(initial_state.clone()).or_default();
        Self {
            id: id.into(),
            initial_state,
            initial_context,
            states,
            state_schemas: HashMap::new(),
            template: None,
            local: false,
            persist: false,
            shared: false,
        }
    }

    /// Declare a state node.  Idempotent — safe to call even if transitions already registered it.
    pub fn state(mut self, name: impl Into<String>) -> Self {
        self.states.entry(name.into()).or_default();
        self
    }

    /// Attach a JSON Schema to a state.  Validated on every entry via `send` or `restore`.
    pub fn schema(mut self, state: impl Into<String>, schema: Value) -> Self {
        self.state_schemas.insert(state.into(), schema);
        self
    }

    /// Embed an HTML template served at `GET /` by `foster_server::router`.
    /// All `fx-show` and `fx-on` attribute values are validated against the machine at startup.
    ///
    /// Use `include_str!("../static/index.html")` to keep the template in a separate file
    /// while still co-locating the reference in the machine definition.
    pub fn template(mut self, html: impl Into<String>) -> Self {
        self.template = Some(html.into());
        self
    }

    /// Run this machine in the browser instead of on the server.
    ///
    /// For per-visitor UI state (a theme toggle, an open dropdown, a lightbox):
    /// a server machine is shared by everyone on the same session, and every
    /// event costs a round trip. A local machine never touches the network —
    /// `foster_server::router` embeds its definition in the served page and
    /// serves no `/state` / `/transition` / `/events` for it.
    ///
    /// Only declarative transitions can run client-side, so every edge must be
    /// `.pass()` or `.merge()`, and `.schema()` isn't supported; `build()`
    /// panics otherwise.
    pub fn local(mut self) -> Self {
        self.local = true;
        self
    }

    /// `.local()`, plus state and context saved to `localStorage` and restored
    /// on the next page load.
    pub fn persist(mut self) -> Self {
        self.local = true;
        self.persist = true;
        self
    }

    /// One server-side instance shared by every visitor: the server ignores the
    /// session for this machine, so a transition (or a `foster_server` feed
    /// update) is pushed to everyone watching. For data that really is global
    /// — a live metrics panel, a background job's status.
    pub fn shared(mut self) -> Self {
        self.shared = true;
        self
    }

    /// Register a transition that shallow-merges the event payload into the context
    /// (see [`merge_shallow`]). Works for server and `.local()` machines alike.
    pub fn merge(
        mut self,
        from: impl Into<String>,
        event: impl Into<String>,
        to: impl Into<String>,
    ) -> Self {
        let def = TransitionDef {
            target: to.into(),
            reduce: Some(Arc::new(|ctx, payload| Ok(merge_shallow(ctx, payload)))),
            local: Some(LocalReduce::Merge),
        };
        self.states.entry(from.into()).or_default().insert(event.into(), def);
        self
    }

    /// Register a list-navigation transition (see [`step_list`]): moves
    /// `ctx[index]` by `by` through `ctx[list]` (wrapping) and merges the
    /// selected item into the context. Works for server and `.local()`
    /// machines alike — e.g. a lightbox's prev/next:
    ///
    /// ```ignore
    /// .step("viewing", "next", "viewing", "photos", "index", 1)
    /// .step("viewing", "prev", "viewing", "photos", "index", -1)
    /// ```
    pub fn step(
        mut self,
        from: impl Into<String>,
        event: impl Into<String>,
        to: impl Into<String>,
        list: impl Into<String>,
        index: impl Into<String>,
        by: i64,
    ) -> Self {
        let reduce = LocalReduce::Step { list: list.into(), index: index.into(), by };
        let for_server = reduce.clone();
        let def = TransitionDef {
            target: to.into(),
            reduce: Some(Arc::new(move |ctx, payload| Ok(for_server.apply(ctx, payload)))),
            local: Some(reduce),
        };
        self.states.entry(from.into()).or_default().insert(event.into(), def);
        self
    }

    /// Register a transition with a reducer.
    ///
    /// Accepts named `fn` pointers and non-capturing closures.
    /// For typed context structs use `.typed_on()`.  For passthrough use `.pass()`.
    pub fn on(
        mut self,
        from: impl Into<String>,
        event: impl Into<String>,
        to: impl Into<String>,
        reduce: impl Fn(Value, Value) -> Result<Value, MachineError> + Send + Sync + 'static,
    ) -> Self {
        let from = from.into();
        let event = event.into();
        self.states
            .entry(from)
            .or_default()
            .insert(event, TransitionDef { target: to.into(), reduce: Some(Arc::new(reduce)), local: None });
        self
    }

    /// Register a passthrough transition — context is forwarded unchanged.
    pub fn pass(
        mut self,
        from: impl Into<String>,
        event: impl Into<String>,
        to: impl Into<String>,
    ) -> Self {
        let from = from.into();
        let event = event.into();
        self.states
            .entry(from)
            .or_default()
            .insert(event, TransitionDef { target: to.into(), reduce: None, local: Some(LocalReduce::Pass) });
        self
    }

    /// Register a transition with a typed-context reducer.
    ///
    /// The reducer receives a deserialized `Ctx` struct and returns an updated one.
    /// Serialization round-trips are handled automatically; a failure is reported as
    /// `MachineError::ContextParse`.
    pub fn typed_on<Ctx>(
        self,
        from: impl Into<String>,
        event: impl Into<String>,
        to: impl Into<String>,
        reduce: fn(Ctx, Value) -> Result<Ctx, MachineError>,
    ) -> Self
    where
        Ctx: Serialize + DeserializeOwned + Send + Sync + 'static,
    {
        self.on(from, event, to, move |ctx: Value, payload: Value| {
            let typed: Ctx = serde_json::from_value(ctx)
                .map_err(|e| MachineError::ContextParse(e.to_string()))?;
            let result = reduce(typed, payload)?;
            serde_json::to_value(result)
                .map_err(|e| MachineError::ContextParse(e.to_string()))
        })
    }

    /// # Panics
    /// For a `.local()` machine with a Rust-reducer edge (`.on()` / `.typed_on()`)
    /// or a `.schema()` — neither can run in the browser.
    pub fn build(mut self) -> Arc<Machine> {
        assert!(
            !(self.local && self.shared),
            "machine '{}': .local() and .shared() are mutually exclusive",
            self.id,
        );
        if self.local {
            for (from, events) in &self.states {
                for (event, def) in events {
                    assert!(
                        def.local.is_some(),
                        "local machine '{}': transition '{from}' -[{event}]-> '{}' has a Rust \
                         reducer, which can't run in the browser — use .pass() or .merge()",
                        self.id, def.target,
                    );
                }
            }
            assert!(
                self.state_schemas.is_empty(),
                "local machine '{}': .schema() isn't supported on local machines",
                self.id,
            );
            // Targets with no outgoing edges must still exist as states, so the
            // client can validate a persisted/server-rendered state against them.
            let targets: Vec<String> =
                self.states.values().flat_map(|e| e.values().map(|d| d.target.clone())).collect();
            for t in targets {
                self.states.entry(t).or_default();
            }
        }
        Arc::new(Machine {
            id: self.id,
            initial_state: self.initial_state,
            initial_context: self.initial_context,
            states: self.states,
            state_schemas: self.state_schemas,
            template: self.template,
            local: self.local,
            persist: self.persist,
            shared: self.shared,
        })
    }
}

/// A live, mutable instance of a machine.  One per user session (or per test).
/// Not `Clone` — snapshot + restore if you need to fork state.
pub struct MachineInstance {
    machine: Arc<Machine>,
    pub current_state: String,
    pub context: Value,
    pub version: u64,
    last_event: Option<String>,
}

impl MachineInstance {
    pub fn new(machine: Arc<Machine>) -> Self {
        let current_state = machine.initial_state.clone();
        let context = machine.initial_context.clone();
        Self { machine, current_state, context, version: 0, last_event: None }
    }

    /// Send an event, advance state, return the resulting snapshot.
    pub fn send(&mut self, event: &str, payload: Value) -> Result<Snapshot, MachineError> {
        let transitions = self
            .machine
            .states
            .get(&self.current_state)
            .ok_or_else(|| MachineError::UnknownState(self.current_state.clone()))?;

        let def = transitions.get(event).ok_or_else(|| MachineError::InvalidTransition {
            state: self.current_state.clone(),
            event: event.to_string(),
        })?;

        let next_context = match &def.reduce {
            Some(f) => f(self.context.clone(), payload)?,
            None => self.context.clone(),
        };

        validate_context(&self.machine, &def.target, &next_context)?;

        self.current_state = def.target.clone();
        self.context = next_context;
        self.version += 1;
        self.last_event = Some(event.to_string());

        Ok(self.snapshot())
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            machine_id: self.machine.id.clone(),
            state: self.current_state.clone(),
            context: self.context.clone(),
            version: self.version,
            last_event: self.last_event.clone(),
        }
    }

    /// Overwrite the instance's state from an arbitrary snapshot.
    pub fn restore(&mut self, snap: Snapshot) -> Result<(), MachineError> {
        if !self.machine.states.contains_key(&snap.state) {
            return Err(MachineError::UnknownState(snap.state));
        }

        validate_context(&self.machine, &snap.state, &snap.context)?;

        self.current_state = snap.state;
        self.context = snap.context;
        self.version = self.version.max(snap.version) + 1;
        self.last_event = None;
        Ok(())
    }

    pub fn valid_events(&self) -> Vec<&str> {
        self.machine
            .states
            .get(&self.current_state)
            .map(|t| t.keys().map(String::as_str).collect())
            .unwrap_or_default()
    }

    pub fn machine(&self) -> &Machine {
        &self.machine
    }
}

// ── Schema validation ─────────────────────────────────────────────────────────

fn validate_context(machine: &Machine, state: &str, context: &Value) -> Result<(), MachineError> {
    if let Some(schema) = machine.state_schemas.get(state) {
        validate_schema(schema, context).map_err(|msg| MachineError::SchemaViolation {
            state: state.to_string(),
            message: msg,
        })?;
    }
    Ok(())
}

fn validate_schema(schema: &Value, instance: &Value) -> Result<(), String> {
    if let Some(ty) = schema.get("type").and_then(Value::as_str) {
        let ok = match ty {
            "object"  => instance.is_object(),
            "array"   => instance.is_array(),
            "string"  => instance.is_string(),
            "number"  => instance.is_number(),
            "integer" => instance.is_i64() || instance.is_u64(),
            "boolean" => instance.is_boolean(),
            "null"    => instance.is_null(),
            _ => true,
        };
        if !ok {
            return Err(format!("expected type '{ty}' but got {}", type_name(instance)));
        }
    }

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for key in required {
            if let Some(k) = key.as_str() {
                if instance.get(k).is_none() {
                    return Err(format!("missing required field '{k}'"));
                }
            }
        }
    }

    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        for (key, sub_schema) in props {
            if let Some(val) = instance.get(key) {
                validate_schema(sub_schema, val).map_err(|e| format!("{key}: {e}"))?;
            }
        }
    }

    if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
        if let Some(n) = instance.as_f64() {
            if n < min {
                return Err(format!("{n} is less than minimum {min}"));
            }
        }
    }
    if let Some(max) = schema.get("maximum").and_then(Value::as_f64) {
        if let Some(n) = instance.as_f64() {
            if n > max {
                return Err(format!("{n} is greater than maximum {max}"));
            }
        }
    }

    if let Some(min_len) = schema.get("minLength").and_then(Value::as_u64) {
        if let Some(s) = instance.as_str() {
            if (s.len() as u64) < min_len {
                return Err(format!("string length {} < minLength {min_len}", s.len()));
            }
        }
    }
    if let Some(max_len) = schema.get("maxLength").and_then(Value::as_u64) {
        if let Some(s) = instance.as_str() {
            if (s.len() as u64) > max_len {
                return Err(format!("string length {} > maxLength {max_len}", s.len()));
            }
        }
    }

    if let Some(variants) = schema.get("enum").and_then(Value::as_array) {
        if !variants.contains(instance) {
            return Err(format!("{instance} is not one of the allowed enum values"));
        }
    }

    Ok(())
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null      => "null",
        Value::Bool(_)   => "boolean",
        Value::Number(n) => if n.is_i64() || n.is_u64() { "integer" } else { "number" },
        Value::String(_) => "string",
        Value::Array(_)  => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use crate::machine_graph;

    // Generate typed enums used by machine_graph tests below.
    machine_graph! {
        id: "test_counter",
        initial: "idle",
        states: ["idle", "error"],
        transitions: [
            ("idle",  "increment", "idle"),
            ("idle",  "break_it",  "error"),
            ("error", "recover",   "idle"),
        ]
    }

    fn counter_machine() -> Arc<Machine> {
        MachineBuilder::new("counter", "idle", json!({ "count": 0 }))
            .state("error")
            .on("idle", "increment", "idle", |ctx, _| {
                Ok(json!({ "count": ctx["count"].as_i64().unwrap_or(0) + 1 }))
            })
            .on("idle", "decrement", "idle", |ctx, _| {
                Ok(json!({ "count": ctx["count"].as_i64().unwrap_or(0) - 1 }))
            })
            .pass("idle", "break_it", "error")
            .pass("error", "recover", "idle")
            .build()
    }

    #[test]
    fn initial_snapshot() {
        let m = MachineInstance::new(counter_machine());
        let s = m.snapshot();
        assert_eq!(s.state, "idle");
        assert_eq!(s.context["count"], 0);
        assert_eq!(s.version, 0);
    }

    #[test]
    fn increment_advances_context() {
        let mut m = MachineInstance::new(counter_machine());
        let s = m.send("increment", json!(null)).unwrap();
        assert_eq!(s.state, "idle");
        assert_eq!(s.context["count"], 1);
        assert_eq!(s.version, 1);
    }

    #[test]
    fn invalid_event_is_error() {
        let mut m = MachineInstance::new(counter_machine());
        assert!(m.send("recover", json!(null)).is_err());
    }

    #[test]
    fn state_transition_and_back() {
        let mut m = MachineInstance::new(counter_machine());
        m.send("increment", json!(null)).unwrap();
        m.send("increment", json!(null)).unwrap();
        m.send("break_it", json!(null)).unwrap();
        assert_eq!(m.current_state, "error");

        let s = m.send("recover", json!(null)).unwrap();
        assert_eq!(s.state, "idle");
        assert_eq!(s.context["count"], 2);
    }

    #[test]
    fn restore_from_snapshot() {
        let machine = counter_machine();
        let mut m = MachineInstance::new(Arc::clone(&machine));

        let injected = Snapshot {
            machine_id: "counter".into(),
            state: "error".into(),
            context: json!({ "count": 99 }),
            version: 42,
            last_event: None,
        };
        m.restore(injected).unwrap();

        assert_eq!(m.current_state, "error");
        assert_eq!(m.context["count"], 99);
        assert_eq!(m.version, 43);
    }

    #[test]
    fn restore_rejects_unknown_state() {
        let mut m = MachineInstance::new(counter_machine());
        let bad = Snapshot {
            machine_id: "counter".into(),
            state: "nonexistent".into(),
            context: json!({}),
            version: 0,
            last_event: None,
        };
        assert!(m.restore(bad).is_err());
    }

    #[test]
    fn machine_enumerates_transitions() {
        let machine = counter_machine();
        let mut triples = machine.transitions();
        triples.sort();
        assert!(triples.contains(&("idle", "increment", "idle")));
        assert!(triples.contains(&("idle", "break_it", "error")));
        assert!(triples.contains(&("error", "recover", "idle")));
    }

    #[test]
    fn schema_rejects_invalid_context() {
        let machine = MachineBuilder::new("m", "ready", json!({ "count": 0 }))
            .schema("ready", json!({
                "type": "object",
                "required": ["count"],
                "properties": { "count": { "type": "integer", "minimum": 0 } }
            }))
            .on("ready", "tick", "ready", |ctx, _| {
                Ok(json!({ "count": ctx["count"].as_i64().unwrap_or(0) + 1 }))
            })
            .build();

        let mut inst = MachineInstance::new(machine);
        assert!(inst.send("tick", json!(null)).is_ok());

        let bad_snap = Snapshot {
            machine_id: "m".into(),
            state: "ready".into(),
            context: json!({ "count": -1 }),
            version: 0,
            last_event: None,
        };
        assert!(inst.restore(bad_snap).is_err());
    }

    #[test]
    fn schema_allows_valid_context() {
        let machine = MachineBuilder::new("m", "ready", json!({ "count": 0 }))
            .schema("ready", json!({
                "type": "object",
                "required": ["count"],
                "properties": { "count": { "type": "integer", "minimum": 0 } }
            }))
            .on("ready", "tick", "ready", |ctx, _| {
                Ok(json!({ "count": ctx["count"].as_i64().unwrap_or(0) + 1 }))
            })
            .build();

        let mut inst = MachineInstance::new(machine);
        let good_snap = Snapshot {
            machine_id: "m".into(),
            state: "ready".into(),
            context: json!({ "count": 5 }),
            version: 10,
            last_event: None,
        };
        assert!(inst.restore(good_snap).is_ok());
        assert_eq!(inst.version, 11);
    }

    #[test]
    fn typed_on_reduces_typed_context() {
        use serde::{Deserialize, Serialize};

        #[derive(Serialize, Deserialize, Clone)]
        struct Ctx {
            count: i64,
        }

        fn increment(mut ctx: Ctx, _: Value) -> Result<Ctx, MachineError> {
            ctx.count += 1;
            Ok(ctx)
        }

        let machine = MachineBuilder::new("typed", "idle", json!({ "count": 0 }))
            .typed_on("idle", "increment", "idle", increment)
            .build();

        let mut inst = MachineInstance::new(machine);
        let snap = inst.send("increment", json!(null)).unwrap();
        assert_eq!(snap.context["count"], 1);
    }

    #[test]
    fn validate_template_catches_unknown_state() {
        let machine = MachineBuilder::new("counter", "idle", json!({}))
            .state("error")
            .pass("idle", "break_it", "error")
            .template(r#"<div fx-machine="counter"><div fx-show="typo"></div></div>"#)
            .build();

        let errs = machine.validate_template().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("'typo'")));
    }

    #[test]
    fn validate_template_catches_unknown_event() {
        let machine = MachineBuilder::new("counter", "idle", json!({}))
            .pass("idle", "reset", "idle")
            .template(r#"<button fx-on="click->typo_event">x</button>"#)
            .build();

        let errs = machine.validate_template().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("'typo_event'")));
    }

    #[test]
    fn validate_template_passes_valid() {
        let machine = MachineBuilder::new("counter", "idle", json!({}))
            .state("error")
            .pass("idle", "break_it", "error")
            .pass("error", "recover", "idle")
            .template(r#"<div fx-show="idle"><button fx-on="click->break_it">x</button></div>"#)
            .build();

        assert!(machine.validate_template().is_ok());
    }

    #[test]
    fn validate_in_scopes_to_own_machine_subtree() {
        // Two distinct machines sharing one page: "theme" only knows
        // light/dark + toggle_theme, "nav" only knows closed/open + toggle_contact.
        // Each must validate cleanly even though the *other* machine's vocabulary
        // appears elsewhere in the same document.
        let html = r#"
            <div fx-machine="theme" fx-class="dark:is-dark">
              <button fx-on="click->toggle_theme">theme</button>
            </div>
            <div fx-machine="nav">
              <button fx-on="click->toggle_contact">contact</button>
              <div fx-show="open">menu</div>
            </div>
        "#;

        let theme = MachineBuilder::new("theme", "light", json!({}))
            .state("dark")
            .pass("light", "toggle_theme", "dark")
            .pass("dark", "toggle_theme", "light")
            .build();
        let nav = MachineBuilder::new("nav", "closed", json!({}))
            .state("open")
            .pass("closed", "toggle_contact", "open")
            .pass("open", "toggle_contact", "closed")
            .build();

        assert!(theme.validate_in(html).is_ok());
        assert!(nav.validate_in(html).is_ok());
    }

    #[test]
    fn validate_in_still_catches_errors_within_own_subtree() {
        let html = r#"
            <div fx-machine="nav">
              <button fx-on="click->toggle_contct">typo</button>
            </div>
        "#;
        let nav = MachineBuilder::new("nav", "closed", json!({}))
            .state("open")
            .pass("closed", "toggle_contact", "open")
            .pass("open", "toggle_contact", "closed")
            .build();

        let errs = nav.validate_in(html).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("'toggle_contct'")));
    }

    #[test]
    fn validate_in_scopes_correctly_when_nested() {
        // A common real layout: a page-wide "theme" machine wraps everything
        // (so its dark-mode class applies to the whole page), with a "nav"
        // machine nested inside it for an unrelated dropdown. theme's own
        // validation must not choke on nav's vocabulary just because nav's
        // markup happens to be a descendant of theme's root.
        let html = r#"
            <div fx-machine="theme" fx-class="dark:is-dark">
              <button fx-on="click->toggle_theme">theme</button>
              <div fx-machine="nav">
                <button fx-on="click->toggle_contact">contact</button>
                <div fx-show="open">menu</div>
              </div>
            </div>
        "#;

        let theme = MachineBuilder::new("theme", "light", json!({}))
            .state("dark")
            .pass("light", "toggle_theme", "dark")
            .pass("dark", "toggle_theme", "light")
            .build();
        let nav = MachineBuilder::new("nav", "closed", json!({}))
            .state("open")
            .pass("closed", "toggle_contact", "open")
            .pass("open", "toggle_contact", "closed")
            .build();

        assert!(theme.validate_in(html).is_ok());
        assert!(nav.validate_in(html).is_ok());
    }

    // ── machine_graph! macro tests ──────────────────────────────────────────

    #[test]
    fn machine_graph_state_as_str() {
        assert_eq!(TestCounterState::Idle.as_str(),  "idle");
        assert_eq!(TestCounterState::Error.as_str(), "error");
    }

    #[test]
    fn machine_graph_event_as_str() {
        assert_eq!(TestCounterEvent::Increment.as_str(), "increment");
        assert_eq!(TestCounterEvent::BreakIt.as_str(),   "break_it");
        assert_eq!(TestCounterEvent::Recover.as_str(),   "recover");
    }

    #[test]
    fn machine_graph_enum_copy_and_eq() {
        let s = TestCounterState::Idle;
        let s2 = s; // Copy
        assert_eq!(s, s2);
        assert_ne!(TestCounterState::Idle, TestCounterState::Error);
    }

    // ── valid_events and last_event tests ────────────────────────────────────

    #[test]
    fn valid_events_from_initial_state() {
        let m = MachineInstance::new(counter_machine());
        let mut events = m.valid_events();
        events.sort();
        assert!(events.contains(&"increment"));
        assert!(events.contains(&"decrement"));
        assert!(events.contains(&"break_it"));
        assert!(!events.contains(&"recover")); // only valid from error state
    }

    #[test]
    fn valid_events_change_after_transition() {
        let mut m = MachineInstance::new(counter_machine());
        m.send("break_it", json!(null)).unwrap();
        let events = m.valid_events();
        assert!(events.contains(&"recover"));
        assert!(!events.contains(&"increment"));
    }

    #[test]
    fn last_event_is_none_on_initial_snapshot() {
        let m = MachineInstance::new(counter_machine());
        assert!(m.snapshot().last_event.is_none());
    }

    #[test]
    fn last_event_is_set_after_send() {
        let mut m = MachineInstance::new(counter_machine());
        let snap = m.send("increment", json!(null)).unwrap();
        assert_eq!(snap.last_event.as_deref(), Some("increment"));
    }

    #[test]
    fn last_event_is_none_after_restore() {
        let mut m = MachineInstance::new(counter_machine());
        m.send("increment", json!(null)).unwrap();
        let snap = Snapshot {
            machine_id: "counter".into(),
            state: "idle".into(),
            context: json!({ "count": 5 }),
            version: 10,
            last_event: None,
        };
        m.restore(snap).unwrap();
        assert!(m.snapshot().last_event.is_none());
    }

    #[test]
    fn version_increments_monotonically() {
        let mut m = MachineInstance::new(counter_machine());
        assert_eq!(m.snapshot().version, 0);
        m.send("increment", json!(null)).unwrap();
        assert_eq!(m.snapshot().version, 1);
        m.send("increment", json!(null)).unwrap();
        assert_eq!(m.snapshot().version, 2);
        m.send("break_it", json!(null)).unwrap();
        assert_eq!(m.snapshot().version, 3);
    }

    // ── local machines ────────────────────────────────────────────────────────

    fn lightbox() -> MachineBuilder {
        MachineBuilder::new("lightbox", "closed", json!({}))
            .merge("closed", "open", "open")
            .pass("open", "close", "closed")
    }

    #[test]
    fn merge_shallow_overwrites_top_level_keys_only() {
        let out = merge_shallow(json!({"a": 1, "n": {"x": 1}}), json!({"a": 2, "n": {"y": 2}}));
        assert_eq!(out, json!({"a": 2, "n": {"y": 2}}));
        assert_eq!(merge_shallow(json!({"a": 1}), json!(null)), json!({"a": 1}));
        assert_eq!(merge_shallow(json!(null), json!({"a": 1})), json!({"a": 1}));
    }

    #[test]
    fn merge_transition_works_on_server_machines() {
        let mut m = MachineInstance::new(lightbox().build());
        let snap = m.send("open", json!({"idx": 3})).unwrap();
        assert_eq!((snap.state.as_str(), snap.context), ("open", json!({"idx": 3})));
    }

    #[test]
    fn local_def_none_for_server_machines() {
        assert!(lightbox().build().local_def().is_none());
    }

    #[test]
    fn local_def_matches_server_semantics() {
        let def = lightbox().persist().build().local_def().unwrap();
        assert!(def.persist);
        assert!(def.has_state("closed") && def.has_state("open"));
        let (st, ctx) = def.send("closed", json!({}), "open", json!({"idx": 3})).unwrap();
        assert_eq!((st.as_str(), &ctx), ("open", &json!({"idx": 3})));
        let (st, ctx) = def.send("open", ctx, "close", json!({"ignored": 1})).unwrap();
        assert_eq!((st.as_str(), ctx), ("closed", json!({"idx": 3})));
        assert!(matches!(
            def.send("closed", json!({}), "close", json!(null)),
            Err(MachineError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn local_def_round_trips_through_json() {
        let def = lightbox().local().build().local_def().unwrap();
        let back: LocalMachineDef = serde_json::from_str(&serde_json::to_string(&def).unwrap()).unwrap();
        assert_eq!(back.transitions["open"]["close"].reduce, LocalReduce::Pass);
        assert_eq!(back.transitions["closed"]["open"].reduce, LocalReduce::Merge);
    }

    #[test]
    fn local_build_registers_terminal_targets_as_states() {
        let def = MachineBuilder::new("m", "a", json!({})).pass("a", "go", "b").local().build()
            .local_def().unwrap();
        assert!(def.has_state("b"));
    }

    #[test]
    #[should_panic(expected = "has a Rust reducer")]
    fn local_build_rejects_rust_reducers() {
        MachineBuilder::new("m", "a", json!({}))
            .on("a", "go", "a", |c, _| Ok(c))
            .local()
            .build();
    }

    #[test]
    #[should_panic(expected = ".schema() isn't supported")]
    fn local_build_rejects_schemas() {
        lightbox().schema("open", json!({"type": "object"})).local().build();
    }

    #[test]
    #[should_panic(expected = "mutually exclusive")]
    fn local_and_shared_rejected() {
        lightbox().local().shared().build();
    }

    #[test]
    fn step_list_wraps_and_merges_item() {
        let ctx = json!({"photos": [{"url": "a"}, {"url": "b"}, {"url": "c"}], "index": 0});
        let next = step_list(ctx.clone(), "photos", "index", 1);
        assert_eq!((next["index"].as_i64(), next["url"].as_str()), (Some(1), Some("b")));
        let wrapped = step_list(ctx.clone(), "photos", "index", -1);
        assert_eq!((wrapped["index"].as_i64(), wrapped["url"].as_str()), (Some(2), Some("c")));
        assert_eq!(step_list(json!({"photos": []}), "photos", "index", 1), json!({"photos": []}));
    }

    #[test]
    fn step_runs_the_same_on_server_and_local() {
        let b = MachineBuilder::new("lb", "viewing", json!({"photos": [{"url": "a"}, {"url": "b"}], "index": 1}))
            .step("viewing", "next", "viewing", "photos", "index", 1);
        let mut server = MachineInstance::new(b.build());
        let s = server.send("next", json!(null)).unwrap();
        let local = MachineBuilder::new("lb", "viewing", json!({"photos": [{"url": "a"}, {"url": "b"}], "index": 1}))
            .step("viewing", "next", "viewing", "photos", "index", 1)
            .local().build().local_def().unwrap();
        let (_, lctx) = local.send("viewing", json!({"photos": [{"url": "a"}, {"url": "b"}], "index": 1}), "next", json!(null)).unwrap();
        assert_eq!(s.context, lctx);
        assert_eq!(lctx["url"], "a");
    }
}
