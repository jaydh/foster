use foster_core::Snapshot;
use futures_util::stream::{self, BoxStream, StreamExt};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::sync::broadcast;

const HISTORY_CAP: usize = 50;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("version conflict: expected {expected}, got {actual}")]
    VersionConflict { expected: u64, actual: u64 },
    #[error("store error: {0}")]
    Backend(String),
}

// ── StateStore ────────────────────────────────────────────────────────────────

/// Persistence layer for machine instances.
///
/// The default impl is [`InMemoryStore`].  A Redis or Postgres impl would
/// satisfy the same interface and make each server replica fully stateless.
///
/// `store` must be atomic: concurrent requests for the same `(session, machine)`
/// pair should use an optimistic lock — reject the write if the snapshot's
/// version has advanced since the `load`.  The `version` field on [`Snapshot`]
/// is the lock token; a Redis Lua CAS script or Postgres `WHERE version = $n`
/// clause is the typical implementation.
pub trait StateStore: Send + Sync + 'static {
    fn load(
        &self,
        session: &str,
        machine: &str,
    ) -> impl Future<Output = Option<Snapshot>> + Send;

    fn store(
        &self,
        session: &str,
        machine: &str,
        snap: &Snapshot,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Return all snapshots recorded for this `(session, machine)` pair, oldest first.
    /// Capped at [`HISTORY_CAP`] entries; earlier snapshots are evicted.
    fn history(
        &self,
        session: &str,
        machine: &str,
    ) -> impl Future<Output = Vec<Snapshot>> + Send;

    /// Atomically load the current snapshot, pass it to `f`, and store the result.
    ///
    /// `f` receives `None` on first access (no prior state for this session).
    /// The load and store are a single atomic unit — no other writer can interleave.
    /// For `InMemoryStore` this is a single Mutex acquisition; for a Redis impl it
    /// would be a Lua CAS script.
    fn apply(
        &self,
        session: &str,
        machine: &str,
        f: impl FnOnce(Option<Snapshot>) -> Result<Snapshot, String> + Send,
    ) -> impl Future<Output = Result<Snapshot, String>> + Send;
}

// ── PubSub ────────────────────────────────────────────────────────────────────

/// Fan-out layer for SSE broadcasts.
///
/// The default impl is [`InMemoryPubSub`], which only reaches SSE connections
/// on the same process.  A Redis pub/sub impl broadcasts across all replicas,
/// so a transition handled by replica A immediately pushes to tabs connected
/// to replica B.
pub trait PubSub: Send + Sync + 'static {
    fn publish(
        &self,
        session: &str,
        machine: &str,
        snap: Snapshot,
    ) -> impl Future<Output = ()> + Send;

    /// Returns a live stream of snapshots for this `(session, machine)` pair.
    /// Each call subscribes from the current moment — past events are not replayed.
    fn subscribe(&self, session: &str, machine: &str) -> BoxStream<'static, Snapshot>;
}

use std::future::Future;

// ── InMemoryStore ─────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct InMemoryStore {
    inner: Arc<Mutex<HashMap<(String, String), Snapshot>>>,
    hist: Arc<Mutex<HashMap<(String, String), VecDeque<Snapshot>>>>,
}

impl StateStore for InMemoryStore {
    async fn load(&self, session: &str, machine: &str) -> Option<Snapshot> {
        self.inner
            .lock()
            .unwrap()
            .get(&(session.to_string(), machine.to_string()))
            .cloned()
    }

    async fn store(&self, session: &str, machine: &str, snap: &Snapshot) -> Result<(), StoreError> {
        let key = (session.to_string(), machine.to_string());
        {
            let mut map = self.inner.lock().unwrap();
            if let Some(existing) = map.get(&key) {
                if existing.version != snap.version.saturating_sub(1) {
                    return Err(StoreError::VersionConflict {
                        expected: snap.version.saturating_sub(1),
                        actual: existing.version,
                    });
                }
            }
            map.insert(key.clone(), snap.clone());
        }
        let mut h = self.hist.lock().unwrap();
        let buf = h.entry(key).or_default();
        buf.push_back(snap.clone());
        if buf.len() > HISTORY_CAP {
            buf.pop_front();
        }
        Ok(())
    }

    async fn history(&self, session: &str, machine: &str) -> Vec<Snapshot> {
        self.hist
            .lock()
            .unwrap()
            .get(&(session.to_string(), machine.to_string()))
            .map(|buf| buf.iter().cloned().collect())
            .unwrap_or_default()
    }

    async fn apply(
        &self,
        session: &str,
        machine: &str,
        f: impl FnOnce(Option<Snapshot>) -> Result<Snapshot, String> + Send,
    ) -> Result<Snapshot, String> {
        let key = (session.to_string(), machine.to_string());
        let next = {
            let mut map = self.inner.lock().unwrap();
            let current = map.get(&key).cloned();
            let next = f(current)?;
            map.insert(key.clone(), next.clone());
            next
        };
        let mut h = self.hist.lock().unwrap();
        let buf = h.entry(key).or_default();
        buf.push_back(next.clone());
        if buf.len() > HISTORY_CAP {
            buf.pop_front();
        }
        Ok(next)
    }
}

// ── InMemoryPubSub ────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct InMemoryPubSub {
    inner: Arc<Mutex<HashMap<(String, String), broadcast::Sender<Snapshot>>>>,
}

impl PubSub for InMemoryPubSub {
    async fn publish(&self, session: &str, machine: &str, snap: Snapshot) {
        let tx = self
            .inner
            .lock()
            .unwrap()
            .get(&(session.to_string(), machine.to_string()))
            .cloned();
        if let Some(tx) = tx {
            let _ = tx.send(snap);
        }
    }

    fn subscribe(&self, session: &str, machine: &str) -> BoxStream<'static, Snapshot> {
        let rx = self
            .inner
            .lock()
            .unwrap()
            .entry((session.to_string(), machine.to_string()))
            .or_insert_with(|| broadcast::channel(64).0)
            .subscribe();

        stream::unfold(rx, |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(snap) => return Some((snap, rx)),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        })
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snap(version: u64) -> Snapshot {
        Snapshot {
            machine_id: "counter".into(),
            state: "idle".into(),
            context: json!({ "count": version }),
            version,
            last_event: None,
        }
    }

    #[tokio::test]
    async fn history_empty_before_any_store() {
        let store = InMemoryStore::default();
        assert!(store.history("s", "counter").await.is_empty());
    }

    #[tokio::test]
    async fn history_records_each_stored_snapshot() {
        let store = InMemoryStore::default();
        store.store("s", "counter", &snap(1)).await.unwrap();
        store.store("s", "counter", &snap(2)).await.unwrap();
        store.store("s", "counter", &snap(3)).await.unwrap();

        let h = store.history("s", "counter").await;
        assert_eq!(h.len(), 3);
        assert_eq!(h[0].version, 1);
        assert_eq!(h[2].version, 3);
    }

    #[tokio::test]
    async fn history_capped_at_history_cap() {
        let store = InMemoryStore::default();
        for i in 1..=(HISTORY_CAP as u64 + 10) {
            store.store("s", "counter", &snap(i)).await.unwrap();
        }
        let h = store.history("s", "counter").await;
        assert_eq!(h.len(), HISTORY_CAP);
        // Oldest evicted; first entry should be entry 11
        assert_eq!(h[0].version, 11);
    }

    #[tokio::test]
    async fn history_is_isolated_by_session_and_machine() {
        let store = InMemoryStore::default();
        store.store("alice", "counter", &snap(1)).await.unwrap();
        store.store("bob", "counter", &snap(1)).await.unwrap();
        store.store("alice", "timer", &snap(1)).await.unwrap();

        assert_eq!(store.history("alice", "counter").await.len(), 1);
        assert_eq!(store.history("bob", "counter").await.len(), 1);
        assert_eq!(store.history("alice", "timer").await.len(), 1);
        assert!(store.history("bob", "timer").await.is_empty());
    }

    #[tokio::test]
    async fn version_conflict_rejected() {
        let store = InMemoryStore::default();
        store.store("s", "counter", &snap(1)).await.unwrap();
        // snap(1) is already stored as version 1; storing version 1 again conflicts
        let err = store.store("s", "counter", &snap(1)).await.unwrap_err();
        assert!(matches!(err, StoreError::VersionConflict { .. }));
    }

    // ── concurrency ───────────────────────────────────────────────────────────
    // This test would have failed against the old separate load()+store() path:
    // both tasks read version 1, both try to write version 2, the second gets
    // a 409.  apply() holds a single lock across the entire read-modify-write
    // so both tasks succeed — the second sees the state written by the first.

    #[tokio::test]
    async fn concurrent_apply_both_succeed_and_versions_are_sequential() {
        let store = Arc::new(InMemoryStore::default());
        // Seed a starting snapshot so both tasks have something to read.
        store.store("s", "counter", &snap(1)).await.unwrap();

        let s1 = Arc::clone(&store);
        let s2 = Arc::clone(&store);

        let t1 = tokio::spawn(async move {
            s1.apply("s", "counter", |current| {
                let v = current.map(|s| s.version).unwrap_or(0);
                Ok(snap(v + 1))
            }).await
        });
        let t2 = tokio::spawn(async move {
            s2.apply("s", "counter", |current| {
                let v = current.map(|s| s.version).unwrap_or(0);
                Ok(snap(v + 1))
            }).await
        });

        let r1 = t1.await.unwrap();
        let r2 = t2.await.unwrap();

        // Neither task should fail — apply() serialises them.
        assert!(r1.is_ok(), "first apply failed: {r1:?}");
        assert!(r2.is_ok(), "second apply failed: {r2:?}");

        // One task wrote v2, the other wrote v3.  Final state must be v3.
        let final_snap = store.load("s", "counter").await.unwrap();
        assert_eq!(final_snap.version, 3);
        // History must contain exactly the two intermediate writes.
        let history = store.history("s", "counter").await;
        assert_eq!(history.len(), 3); // seed + two applies
        assert_eq!(history[0].version, 1);
        assert_eq!(history[2].version, 3);
    }
}
