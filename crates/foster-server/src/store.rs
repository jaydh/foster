use foster_core::Snapshot;
use futures_util::stream::{self, BoxStream, StreamExt};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::sync::broadcast;

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
        let mut map = self.inner.lock().unwrap();
        if let Some(existing) = map.get(&key) {
            if existing.version != snap.version.saturating_sub(1) {
                return Err(StoreError::VersionConflict {
                    expected: snap.version.saturating_sub(1),
                    actual: existing.version,
                });
            }
        }
        map.insert(key, snap.clone());
        Ok(())
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
