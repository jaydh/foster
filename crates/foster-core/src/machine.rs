use crate::snapshot::Snapshot;
use serde::de::DeserializeOwned;
use serde::Serialize;
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
}

impl Machine {
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
    pub fn validate_template(&self) -> Result<(), Vec<String>> {
        let html = match &self.template {
            Some(h) => h,
            None => return Ok(()),
        };

        let valid_states: std::collections::HashSet<&str> =
            self.states.keys().map(|s| s.as_str()).collect();
        let valid_events: std::collections::HashSet<&str> = self
            .states
            .values()
            .flat_map(|events| events.keys().map(|e| e.as_str()))
            .collect();

        let mut errors = Vec::new();

        for val in extract_attr_values(html, "fx-show") {
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

        for val in extract_attr_values(html, "fx-on") {
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

/// Builder for `Machine`.  All methods consume and return `Self` for chaining.
pub struct MachineBuilder {
    id: String,
    initial_state: String,
    initial_context: Value,
    states: HashMap<String, HashMap<String, TransitionDef>>,
    state_schemas: HashMap<String, Value>,
    template: Option<String>,
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
            .insert(event, TransitionDef { target: to.into(), reduce: Some(Arc::new(reduce)) });
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
            .insert(event, TransitionDef { target: to.into(), reduce: None });
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

    pub fn build(self) -> Arc<Machine> {
        Arc::new(Machine {
            id: self.id,
            initial_state: self.initial_state,
            initial_context: self.initial_context,
            states: self.states,
            state_schemas: self.state_schemas,
            template: self.template,
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
}
