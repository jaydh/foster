use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The full serializable state of a machine instance at a point in time.
/// This is the unit of time-travel, test injection, and state sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub machine_id: String,
    /// Name of the active state node (e.g. "idle", "error").
    pub state: String,
    /// Arbitrary context data associated with this machine instance.
    pub context: Value,
    /// Monotonically increasing version counter — used by the client
    /// to detect stale snapshots and by tests to await specific transitions.
    pub version: u64,
}
