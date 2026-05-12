use crate::snapshot::Snapshot;
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
}

/// A single edge in the state graph.
pub struct TransitionDef {
    /// Target state name after this transition fires.
    pub target: String,
    /// Optional pure function that produces new context from (old_context, event_payload).
    /// `None` means context passes through unchanged.
    /// `fn` pointers only — no closures — so the machine definition is Send + Sync.
    pub reduce: Option<fn(Value, Value) -> Result<Value, MachineError>>,
}

/// The static, shared definition of a machine.  Construct via `MachineBuilder`.
/// `Arc<Machine>` is cheaply cloneable and can be shared across threads.
pub struct Machine {
    pub id: String,
    pub initial_state: String,
    pub initial_context: Value,
    /// state_name → event_name → TransitionDef
    pub(crate) states: HashMap<String, HashMap<String, TransitionDef>>,
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
}

/// Builder for `Machine`.  All methods consume and return `Self` for chaining.
pub struct MachineBuilder {
    id: String,
    initial_state: String,
    initial_context: Value,
    states: HashMap<String, HashMap<String, TransitionDef>>,
}

impl MachineBuilder {
    pub fn new(
        id: impl Into<String>,
        initial_state: impl Into<String>,
        initial_context: Value,
    ) -> Self {
        let initial_state = initial_state.into();
        let mut states: HashMap<String, HashMap<String, TransitionDef>> = HashMap::new();
        // Pre-register the initial state so it shows up even with no transitions defined.
        states.entry(initial_state.clone()).or_default();
        Self {
            id: id.into(),
            initial_state,
            initial_context,
            states,
        }
    }

    /// Declare a state node.  Idempotent — safe to call even if transitions already registered it.
    pub fn state(mut self, name: impl Into<String>) -> Self {
        self.states.entry(name.into()).or_default();
        self
    }

    /// Register a transition edge.
    ///
    /// `reduce` receives `(current_context, event_payload)` and returns the next context.
    /// Pass `None` to leave context unchanged.
    pub fn on(
        mut self,
        from: impl Into<String>,
        event: impl Into<String>,
        to: impl Into<String>,
        reduce: Option<fn(Value, Value) -> Result<Value, MachineError>>,
    ) -> Self {
        let from = from.into();
        let event = event.into();
        self.states
            .entry(from)
            .or_default()
            .insert(event, TransitionDef { target: to.into(), reduce });
        self
    }

    pub fn build(self) -> Arc<Machine> {
        Arc::new(Machine {
            id: self.id,
            initial_state: self.initial_state,
            initial_context: self.initial_context,
            states: self.states,
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
}

impl MachineInstance {
    pub fn new(machine: Arc<Machine>) -> Self {
        let current_state = machine.initial_state.clone();
        let context = machine.initial_context.clone();
        Self { machine, current_state, context, version: 0 }
    }

    /// Send an event, advance state, return the resulting snapshot.
    /// Returns `MachineError::InvalidTransition` for events not valid in the current state —
    /// the caller decides whether to propagate or ignore.
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

        let next_context = match def.reduce {
            Some(f) => f(self.context.clone(), payload)?,
            None => self.context.clone(),
        };

        self.current_state = def.target.clone();
        self.context = next_context;
        self.version += 1;

        Ok(self.snapshot())
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            machine_id: self.machine.id.clone(),
            state: self.current_state.clone(),
            context: self.context.clone(),
            version: self.version,
        }
    }

    /// Overwrite the instance's state from an arbitrary snapshot.
    /// Used by `POST /test/state` to inject test fixtures without replaying interactions.
    pub fn restore(&mut self, snap: Snapshot) -> Result<(), MachineError> {
        if !self.machine.states.contains_key(&snap.state) {
            return Err(MachineError::UnknownState(snap.state));
        }
        self.current_state = snap.state;
        self.context = snap.context;
        self.version = snap.version;
        Ok(())
    }

    /// Events valid from the current state — exposed so the server can surface them
    /// in responses and test generators can enumerate reachable transitions.
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn counter_machine() -> Arc<Machine> {
        MachineBuilder::new("counter", "idle", json!({ "count": 0 }))
            .state("error")
            .on("idle", "increment", "idle", Some(|ctx, _| {
                Ok(json!({ "count": ctx["count"].as_i64().unwrap_or(0) + 1 }))
            }))
            .on("idle", "decrement", "idle", Some(|ctx, _| {
                Ok(json!({ "count": ctx["count"].as_i64().unwrap_or(0) - 1 }))
            }))
            .on("idle", "break_it", "error", None)
            .on("error", "recover", "idle", Some(|ctx, _| Ok(ctx)))
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
        // context passes through on recover
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
        };
        m.restore(injected).unwrap();

        assert_eq!(m.current_state, "error");
        assert_eq!(m.context["count"], 99);
        assert_eq!(m.version, 42);
    }

    #[test]
    fn restore_rejects_unknown_state() {
        let mut m = MachineInstance::new(counter_machine());
        let bad = Snapshot {
            machine_id: "counter".into(),
            state: "nonexistent".into(),
            context: json!({}),
            version: 0,
        };
        assert!(m.restore(bad).is_err());
    }

    #[test]
    fn machine_enumerates_transitions() {
        let machine = counter_machine();
        let mut triples = machine.transitions();
        triples.sort();
        // Should include all defined edges
        assert!(triples.contains(&("idle", "increment", "idle")));
        assert!(triples.contains(&("idle", "break_it", "error")));
        assert!(triples.contains(&("error", "recover", "idle")));
    }
}
