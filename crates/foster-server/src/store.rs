use foster_core::Snapshot;
use futures_util::stream::{self, BoxStream, StreamExt};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
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
/// The default impl is [`InMemoryStore`]. A shared backend (Redis,
/// Postgres) would satisfy the same interface and make each server replica
/// stateless.
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

    /// Drop instances not read or written for `idle`, unless `keep(session,
    /// machine)` (a page is still watching it). Returns how many went.
    /// Stores with their own expiry (Redis keys with a TTL) can leave this
    /// as the default no-op.
    fn evict_idle(
        &self,
        idle: Duration,
        keep: &(dyn Fn(&str, &str) -> bool + Send + Sync),
    ) -> impl Future<Output = usize> + Send {
        let _ = (idle, keep);
        async { 0 }
    }
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

    /// Whether a page on this process is subscribed to the pair right now
    /// (keeps its instance from [idle eviction](StateStore::evict_idle)).
    fn watched(&self, session: &str, machine: &str) -> bool {
        let _ = (session, machine);
        false
    }
}

use std::future::Future;

// ── InMemoryStore ─────────────────────────────────────────────────────────────

/// Instances in a `HashMap`, each with its last-access time (for
/// [`StateStore::evict_idle`]). The last [`HISTORY_CAP`] snapshots of each
/// are kept for the debug endpoints only when `history` is on — by default
/// in debug builds and with `FOSTER_TEST_MODE=1`: a release server keeping
/// 50 copies of every context ran to ~500 MB per session on a page with a
/// 2,000-row list.
/// `(session, machine)`.
type Key = (String, String);
type Shared<T> = Arc<Mutex<HashMap<Key, T>>>;

#[derive(Clone)]
pub struct InMemoryStore {
    inner: Shared<(Snapshot, Instant)>,
    hist: Shared<VecDeque<Snapshot>>,
    history: bool,
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::with_history(crate::debug_mode())
    }
}

impl InMemoryStore {
    /// A store that does (or doesn't) keep each instance's recent snapshots.
    pub fn with_history(history: bool) -> Self {
        Self { inner: Default::default(), hist: Default::default(), history }
    }

    fn record(&self, key: (String, String), snap: &Snapshot) {
        if !self.history {
            return;
        }
        let mut h = self.hist.lock().unwrap();
        let buf = h.entry(key).or_default();
        buf.push_back(snap.clone());
        if buf.len() > HISTORY_CAP {
            buf.pop_front();
        }
    }
}

impl StateStore for InMemoryStore {
    async fn load(&self, session: &str, machine: &str) -> Option<Snapshot> {
        let mut map = self.inner.lock().unwrap();
        let (snap, touched) = map.get_mut(&(session.to_string(), machine.to_string()))?;
        *touched = Instant::now();
        Some(snap.clone())
    }

    async fn store(&self, session: &str, machine: &str, snap: &Snapshot) -> Result<(), StoreError> {
        let key = (session.to_string(), machine.to_string());
        {
            let mut map = self.inner.lock().unwrap();
            if let Some((existing, _)) = map.get(&key) {
                if existing.version != snap.version.saturating_sub(1) {
                    return Err(StoreError::VersionConflict {
                        expected: snap.version.saturating_sub(1),
                        actual: existing.version,
                    });
                }
            }
            map.insert(key.clone(), (snap.clone(), Instant::now()));
        }
        self.record(key, snap);
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

        // Read the current snapshot, then release the lock *before* calling `f`.
        //
        // The previous version held `self.inner`'s std::sync::Mutex across the
        // entire call to `f` — fine as long as every reducer was a pure,
        // instant JSON transform, which was true of every machine in this
        // workspace until reducers started doing real I/O via
        // `tokio::task::block_in_place` (see examples/jaydanhoward's
        // cluster/visitors machines). One global `std::sync::Mutex` blocks
        // *every* session/machine pair, not just this one — so a single slow
        // reducer stalls every other machine's `/state` and `/transition` on
        // the whole server. Worse: `block_in_place` needs a free tokio worker
        // thread to drive the async I/O it's waiting on; if enough other
        // requests pile up blocked on this same std Mutex, the worker pool
        // can exhaust entirely and the reducer's own future never gets
        // polled — a permanent deadlock, not just a slowdown. Reproduced
        // live: a "refresh" button whose reducer queries Postgres/kube hung
        // forever with 7 machines' worth of concurrent SSE connections open.
        //
        // Releasing the lock during `f` fixes the deadlock; re-checking the
        // version on write (matching `store()`'s optimistic-lock contract)
        // keeps the "no other writer can interleave" guarantee — a
        // concurrent write during a slow reducer is now a detected conflict
        // instead of a silent clobber.
        let current = {
            let map = self.inner.lock().unwrap();
            map.get(&key).map(|(s, _)| s.clone())
        };
        let before_version = current.as_ref().map(|s| s.version);
        let next = f(current)?;

        {
            let mut map = self.inner.lock().unwrap();
            let actual_version = map.get(&key).map(|(s, _)| s.version);
            if actual_version != before_version {
                return Err(format!(
                    "concurrent write to ({session}, {machine}) during reducer: expected version {before_version:?}, found {actual_version:?}"
                ));
            }
            map.insert(key.clone(), (next.clone(), Instant::now()));
        }
        self.record(key, &next);
        Ok(next)
    }

    async fn evict_idle(&self, idle: Duration, keep: &(dyn Fn(&str, &str) -> bool + Send + Sync)) -> usize {
        let gone: Vec<(String, String)> = {
            let mut map = self.inner.lock().unwrap();
            let gone: Vec<_> = map
                .iter()
                .filter(|((s, m), (_, touched))| touched.elapsed() >= idle && !keep(s, m))
                .map(|(k, _)| k.clone())
                .collect();
            for k in &gone {
                map.remove(k);
            }
            gone
        };
        let mut h = self.hist.lock().unwrap();
        for k in &gone {
            h.remove(k);
        }
        gone.len()
    }
}

// ── InMemoryPubSub ────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct InMemoryPubSub {
    inner: Shared<broadcast::Sender<Snapshot>>,
}

impl PubSub for InMemoryPubSub {
    fn watched(&self, session: &str, machine: &str) -> bool {
        let mut map = self.inner.lock().unwrap();
        let key = (session.to_string(), machine.to_string());
        let watched = map.get(&key).is_some_and(|tx| tx.receiver_count() > 0);
        if !watched {
            // Nobody's listening: forget the channel too.
            map.remove(&key);
        }
        watched
    }

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
            context: json!({ "count": version }).into(),
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
    async fn history_only_when_asked_for() {
        let store = InMemoryStore::with_history(false);
        store.store("s", "counter", &snap(1)).await.unwrap();
        store.apply("s", "counter", |_| Ok(snap(2))).await.unwrap();
        assert!(store.history("s", "counter").await.is_empty());
        assert_eq!(store.load("s", "counter").await.unwrap().version, 2);
    }

    #[tokio::test]
    async fn evict_idle_drops_old_unkept_instances() {
        let store = InMemoryStore::with_history(true);
        for s in ["old", "kept"] {
            store.store(s, "counter", &snap(1)).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        store.store("fresh", "counter", &snap(1)).await.unwrap();
        let n = store.evict_idle(Duration::from_millis(20), &|s, _| s == "kept").await;
        assert_eq!(n, 1);
        assert!(store.load("old", "counter").await.is_none());
        assert!(store.history("old", "counter").await.is_empty());
        assert!(store.load("kept", "counter").await.is_some());
        assert!(store.load("fresh", "counter").await.is_some());
    }

    #[tokio::test]
    async fn watched_while_subscribed() {
        let ps = InMemoryPubSub::default();
        assert!(!ps.watched("s", "m"));
        let sub = ps.subscribe("s", "m");
        assert!(ps.watched("s", "m"));
        drop(sub);
        assert!(!ps.watched("s", "m"));
    }

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

    // Regression test for a real deadlock found while building a reducer that
    // does actual I/O (kube/Postgres queries via `block_in_place`, in
    // examples/jaydanhoward's cluster/visitors machines): `apply()` used to
    // hold `self.inner`'s std::sync::Mutex across the *entire* call to `f`,
    // and that mutex is global across every session/machine pair, not
    // scoped to the one being applied. A slow reducer therefore stalled
    // every other machine's `load`/`store`/`apply` server-wide, and with
    // enough concurrent requests piled up on that one lock, the tokio
    // worker pool could exhaust entirely — a permanent hang, reproduced
    // live with 7 machines' worth of concurrent SSE connections open.
    #[tokio::test]
    async fn slow_reducer_does_not_block_unrelated_operations() {
        let store = Arc::new(InMemoryStore::default());
        store.store("s", "slow", &snap(1)).await.unwrap();
        store.store("s", "other", &snap(1)).await.unwrap();

        let s1 = Arc::clone(&store);
        let slow_apply = tokio::spawn(async move {
            s1.apply("s", "slow", |current| {
                // Simulates a reducer blocking on real I/O (the actual bug
                // used `block_in_place` + a real kube/Postgres call; a plain
                // OS-thread sleep reproduces the same "holds the lock for a
                // while" shape without needing real network/DB access).
                std::thread::sleep(std::time::Duration::from_millis(300));
                let v = current.map(|s| s.version).unwrap_or(0);
                Ok(snap(v + 1))
            })
            .await
        });

        // Give the slow apply a moment to start.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let start = std::time::Instant::now();
        let other = store.load("s", "other").await;
        let elapsed = start.elapsed();

        assert!(other.is_some());
        assert!(
            elapsed < std::time::Duration::from_millis(250),
            "load() on an unrelated key blocked behind a slow reducer for {elapsed:?} — \
             the global lock is being held across `f`, the deadlock this test guards against"
        );

        slow_apply.await.unwrap().unwrap();
    }
}
