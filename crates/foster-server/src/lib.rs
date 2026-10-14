pub mod store;
pub use store::{InMemoryPubSub, InMemoryStore, PubSub, StateStore, StoreError};
#[cfg(feature = "redis-backend")]
pub use store::{RedisStore, RedisPubSub};

use axum::{
    body::{Body, Bytes},
    extract::{ConnectInfo, Extension, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, sse::{Event, KeepAlive, Sse}, IntoResponse, Response},
    middleware::{self, Next},
    routing::{get, post},
    Json, Router,
};
use axum::extract::DefaultBodyLimit;
use foster_core::{merge_shallow, Machine, MachineInstance, Snapshot};
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use futures_util::{FutureExt, Stream, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::watch;

/// Session key every `.shared()` machine is stored and broadcast under.
const SHARED_SESSION: &str = "__shared__";

// ── state ─────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState<S = InMemoryStore, P = InMemoryPubSub>
where
    S: StateStore + Clone,
    P: PubSub + Clone,
{
    machines: Arc<HashMap<String, Arc<Machine>>>,
    store: S,
    pubsub: P,
    test_mode: bool,
    /// `.shared()` machine id → number of open `/events` streams, so feeds can
    /// pause while nobody is watching.
    watchers: Arc<HashMap<String, watch::Sender<usize>>>,
    request_events: Arc<HashMap<String, RequestEvent>>,
    /// machine id → event → handler run after that transition — see [`Foster::effect`].
    effects: Arc<HashMap<String, HashMap<String, EffectFn>>>,
    /// Whether [`Foster::identity`] is set, i.e. session keys are scoped by user.
    scoped: bool,
}

/// The authenticated user of a request, from [`Foster::identity`]; put in the
/// request's extensions by the identity middleware.
#[derive(Clone, Debug)]
struct Identity(String);

impl<S: StateStore + Clone, P: PubSub + Clone> AppState<S, P> {
    /// The store/pubsub key for `machine`: [`SHARED_SESSION`] for `.shared()`
    /// machines (one instance for everyone), else the caller's session —
    /// prefixed with their identity when [`Foster::identity`] is set, so a
    /// session ID is only meaningful to the user it belongs to.
    fn session(&self, machine: &str, requested: &str, who: Option<&Identity>) -> String {
        if self.watchers.contains_key(machine) {
            SHARED_SESSION.to_string()
        } else {
            match who {
                // Length-prefixed, so no (identity, session) pair can collide
                // with another's however either is spelled.
                Some(Identity(id)) => format!("{}:{id}:{requested}", id.len()),
                None => requested.to_string(),
            }
        }
    }

    /// Inverse of [`session`](Self::session) for a per-session key:
    /// (identity, client session).
    fn split_key<'a>(&self, key: &'a str) -> (Option<&'a str>, &'a str) {
        if !self.scoped || key == SHARED_SESSION {
            return (None, key);
        }
        let parsed = key.split_once(':').and_then(|(len, rest)| {
            let len: usize = len.parse().ok()?;
            let id = rest.get(..len)?;
            Some((id, rest.get(len + 1..)?))
        });
        match parsed {
            Some((id, session)) => (Some(id), session),
            None => (None, key),
        }
    }

    /// Fire `event` on `(session, machine_id)` atomically, broadcast the result,
    /// and start the event's [effect](Foster::effect), if any.
    async fn fire(&self, session: &str, machine_id: &str, event: &str, payload: Value) -> Result<Snapshot, String> {
        let machine = self
            .machines
            .get(machine_id)
            .cloned()
            .ok_or_else(|| format!("machine '{machine_id}' not found"))?;
        let effect = self.effects.get(machine_id).and_then(|m| m.get(event)).cloned();
        let effect_payload = effect.is_some().then(|| payload.clone());
        let shared = self.watchers.contains_key(machine_id);
        // The store rejects a write that raced another one to the same
        // instance (two effects reporting at once, a click during a job's
        // progress update). Reducers are pure, so run it again on the newer
        // state rather than dropping the event.
        let mut attempt = 0;
        let snap = loop {
            let (machine, payload, event_owned) = (machine.clone(), payload.clone(), event.to_string());
            let result = self
                .store
                .apply(session, machine_id, move |current| {
                    let mut inst = MachineInstance::new(machine);
                    if let Some(snap) = current {
                        inst.restore(snap).map_err(|e| e.to_string())?;
                    }
                    let mut snap = inst.send(&event_owned, payload).map_err(|e| e.to_string())?;
                    // Shared machines can live on several replicas at once (each
                    // with its own in-memory store, each driven by its own feed),
                    // and a page may read /state from one replica and stream
                    // /events from another. Per-replica counters diverge (one
                    // replica at v17000, another at v4000), and the client drops
                    // every "older" update. Wall-clock versions keep replicas
                    // comparable while staying monotonic per replica.
                    if shared {
                        snap.version = snap.version.max(unix_millis());
                    }
                    Ok(snap)
                })
                .await;
            match result {
                Err(e) if is_conflict(&e) && attempt < CONFLICT_RETRIES => attempt += 1,
                other => break other?,
            }
        };
        self.pubsub.publish(session, machine_id, snap.clone()).await;
        if let (Some(effect), Some(payload)) = (effect, effect_payload) {
            let (identity, client_session) = self.split_key(session);
            tokio::spawn(effect(Effect {
                session: client_session.to_string(),
                identity: identity.map(str::to_string),
                key: session.to_string(),
                machine: machine_id.to_string(),
                event: event.to_string(),
                payload,
                snapshot: snap.clone(),
                fire: self.fire_fn(),
            }));
        }
        Ok(snap)
    }

    /// [`fire`](Self::fire), type-erased for [`Effect::send`]. The target
    /// machine's session is resolved here, so an effect can reach a
    /// `.shared()` machine as well as its own session's machines.
    fn fire_fn(&self) -> FireFn {
        let app = self.clone();
        // `key` is an already-scoped per-session key (an effect's own); only
        // `.shared()` targets need remapping.
        Arc::new(move |key, machine, event, payload| {
            let app = app.clone();
            async move {
                let key = if app.watchers.contains_key(&machine) { SHARED_SESSION.to_string() } else { key };
                app.fire(&key, &machine, &event, payload).await
            }
            .boxed()
        })
    }
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── request events & feeds ────────────────────────────────────────────────────

/// What a request-event handler gets to look at: the HTTP request behind a
/// `GET /state` or `POST /transition` (headers + peer address, when the app is
/// served with `into_make_service_with_connect_info::<SocketAddr>()`).
#[derive(Clone, Debug)]
pub struct RequestInfo {
    pub headers: HeaderMap,
    pub remote_addr: Option<SocketAddr>,
    /// The authenticated user, when [`Foster::identity`] is set (`None` when
    /// it's the identity function itself being called).
    pub identity: Option<String>,
}

type IdentityFn = Arc<dyn Fn(&RequestInfo) -> Option<String> + Send + Sync>;

type RequestFn = Arc<dyn Fn(RequestInfo) -> BoxFuture<'static, Value> + Send + Sync>;

#[derive(Clone)]
struct RequestEvent {
    event: String,
    handler: RequestFn,
}

struct Feed {
    machine: String,
    event: String,
    stream: BoxStream<'static, Value>,
}

type EffectFn = Arc<dyn Fn(Effect) -> BoxFuture<'static, ()> + Send + Sync>;
type FireFn =
    Arc<dyn Fn(String, String, String, Value) -> BoxFuture<'static, Result<Snapshot, String>> + Send + Sync>;

/// What an [effect](Foster::effect) handler gets: the transition that triggered
/// it, and a way to send follow-up events into the same session.
#[derive(Clone)]
pub struct Effect {
    /// The session the triggering event came from (the client's random session
    /// ID; `__shared__` for `.shared()` machines). Useful as a key for
    /// per-session server resources, such as a job a later event cancels — not
    /// an identity: it's client-generated. Pair it with `identity` when
    /// [`Foster::identity`] is set.
    pub session: String,
    /// The authenticated user whose session this is, when [`Foster::identity`]
    /// is set.
    pub identity: Option<String>,
    /// Store key of the session — what follow-up events are sent to.
    key: String,
    pub machine: String,
    pub event: String,
    /// The triggering event's payload (after any request-event merge).
    pub payload: Value,
    /// The machine's snapshot right after the triggering transition.
    pub snapshot: Snapshot,
    fire: FireFn,
}

/// How many times [`Foster`] reruns a transition whose write raced another.
const CONFLICT_RETRIES: usize = 16;

/// A store's "someone else wrote first" error (in-memory or Redis).
fn is_conflict(err: &str) -> bool {
    err.starts_with("concurrent write") || err.contains("version_conflict") || err.contains("version conflict")
}

impl Effect {
    /// Send `event` to the same machine instance — a result, a progress
    /// update, an error. It goes through the reducer like any transition and is
    /// pushed to the session's open pages; it can trigger its own effect.
    pub async fn send(&self, event: &str, payload: Value) -> Result<Snapshot, String> {
        (self.fire)(self.key.clone(), self.machine.clone(), event.to_string(), payload).await
    }

    /// Send `event` to another machine in the same session (or to a
    /// `.shared()` machine).
    pub async fn send_to(&self, machine: &str, event: &str, payload: Value) -> Result<Snapshot, String> {
        (self.fire)(self.key.clone(), machine.to_string(), event.to_string(), payload).await
    }
}

/// Builder for the Foster router when you need more than [`router`]: server-side
/// [feeds](Foster::feed) and [request events](Foster::request_event).
///
/// ```ignore
/// let app = foster_server::Foster::new(machines)
///     .feed("cluster", "tick", cluster_snapshots)        // Stream<Item = Value>
///     .request_event("trace", "trace", |req| async move { trace_of(req).await })
///     .router();
/// ```
pub struct Foster {
    machines: HashMap<String, Arc<Machine>>,
    feeds: Vec<Feed>,
    request_events: HashMap<String, RequestEvent>,
    effects: HashMap<String, HashMap<String, EffectFn>>,
    identity: Option<IdentityFn>,
}

impl Foster {
    pub fn new(machines: HashMap<String, Arc<Machine>>) -> Self {
        Self { machines, feeds: Vec::new(), request_events: HashMap::new(), effects: HashMap::new(), identity: None }
    }

    /// Require an authenticated user on every Foster route (the page, `/state`,
    /// `/transition`, `/events`, debug routes). `identify` maps a request to its
    /// user — typically from a header set by a trusted auth proxy — or `None`,
    /// which gets `401 Unauthorized`.
    ///
    /// Session state is then stored per user: a session ID only addresses that
    /// user's machines, so knowing someone else's (client-generated) session ID
    /// gets you nothing. Effects and request events see the user as `identity`.
    /// `.shared()` machines stay one instance for everyone.
    ///
    /// ```ignore
    /// .identity(|req| {
    ///     let user = req.headers.get("tailscale-user-login")?.to_str().ok()?;
    ///     ALLOWED.contains(&user).then(|| user.to_string())
    /// })
    /// ```
    ///
    /// Only trust a header if the app is reachable solely through the proxy
    /// that sets it.
    pub fn identity<F>(mut self, identify: F) -> Self
    where
        F: Fn(&RequestInfo) -> Option<String> + Send + Sync + 'static,
    {
        self.identity = Some(Arc::new(identify));
        self
    }

    /// Run async server-side work after `machine` takes `event`: a database
    /// query, a filesystem operation, a long job reporting progress. `handler`
    /// is spawned once the transition is committed and broadcast, and reports
    /// back by sending events into the same session with [`Effect::send`] —
    /// so the reducers stay pure and the result reaches every open page over
    /// SSE.
    ///
    /// ```ignore
    /// // "open" moves the machine to "loading"; the effect fills it in.
    /// .effect("dir", "open", |fx: Effect| async move {
    ///     let listing = list_dir(fx.payload["path"].as_str().unwrap_or("/")).await;
    ///     let _ = match listing {
    ///         Ok(entries) => fx.send("loaded", json!({ "entries": entries })).await,
    ///         Err(e) => fx.send("failed", json!({ "error": e.to_string() })).await,
    ///     };
    /// })
    /// ```
    ///
    /// Fires for every successful `event` on the machine: client transitions,
    /// feed items, and events sent by other effects. One handler per
    /// `(machine, event)`; a later call replaces an earlier one.
    ///
    /// # Panics
    /// When `router()` is built: if `machine` is unknown or `.local()`, or never
    /// handles `event`. Requires a Tokio runtime.
    pub fn effect<F, Fut>(mut self, machine: &str, event: &str, handler: F) -> Self
    where
        F: Fn(Effect) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let handler: EffectFn = Arc::new(move |fx| handler(fx).boxed());
        self.effects.entry(machine.into()).or_default().insert(event.into(), handler);
        self
    }

    /// Drive a `.shared()` machine from server-side data: each item `stream`
    /// yields is sent to the machine as `event` with the item as payload (so it
    /// goes through the machine's reducer and schema like any transition), then
    /// pushed to every viewer.
    ///
    /// The stream is only polled while at least one client has the machine's
    /// `/events` stream open — a polling stream (`IntervalStream` + `then`)
    /// does no work while nobody is looking.
    ///
    /// # Panics
    /// When `router()` is built: if `machine` isn't `.shared()` or never handles
    /// `event`. Requires a Tokio runtime (the feed runs as a spawned task).
    pub fn feed<St>(mut self, machine: &str, event: &str, stream: St) -> Self
    where
        St: Stream<Item = Value> + Send + 'static,
    {
        self.feeds.push(Feed { machine: machine.into(), event: event.into(), stream: stream.boxed() });
        self
    }

    /// Feed request-derived data into a (per-session) machine. `handler`'s
    /// result is shallow-merged into the payload of `event`, and:
    /// - every `GET /state` for `machine` fires `event` first, so each page load
    ///   renders fresh request data;
    /// - a client-sent `event` (e.g. a refresh button) gets it too.
    pub fn request_event<F, Fut>(mut self, machine: &str, event: &str, handler: F) -> Self
    where
        F: Fn(RequestInfo) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Value> + Send + 'static,
    {
        let handler: RequestFn = Arc::new(move |req| handler(req).boxed());
        self.request_events.insert(machine.into(), RequestEvent { event: event.into(), handler });
        self
    }

    /// Router with in-memory store and pubsub.
    pub fn router(self) -> Router {
        self.router_with(InMemoryStore::default(), InMemoryPubSub::default())
    }

    /// Router with explicit backends — see [`router_with`].
    pub fn router_with<S, P>(self, store: S, pubsub: P) -> Router
    where
        S: StateStore + Clone + 'static,
        P: PubSub + Clone + 'static,
    {
        build_router(self, store, pubsub)
    }
}

/// Run one feed: wait for a watcher, pull the next item, fire it; repeat.
fn spawn_feed<S, P>(app: AppState<S, P>, feed: Feed)
where
    S: StateStore + Clone + 'static,
    P: PubSub + Clone + 'static,
{
    let mut watchers = app.watchers[&feed.machine].subscribe();
    let Feed { machine, event, mut stream } = feed;
    tokio::spawn(async move {
        loop {
            if watchers.wait_for(|n| *n > 0).await.is_err() {
                return;
            }
            let Some(payload) = stream.next().await else { return };
            if let Err(e) = app.fire(SHARED_SESSION, &machine, &event, payload).await {
                eprintln!("foster: feed '{machine}' -[{event}]-> failed: {e}");
            }
        }
    });
}

/// Decrements a shared machine's watcher count when an `/events` stream closes.
struct WatchGuard(watch::Sender<usize>);

impl Drop for WatchGuard {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n = n.saturating_sub(1));
    }
}

// ── router ────────────────────────────────────────────────────────────────────

/// Build the Foster API router with in-memory store and pubsub (default, single-process).
///
/// For multi-replica deployments, use [`router_with`] and supply Redis-backed
/// [`StateStore`] and [`PubSub`] implementations so all replicas share state and
/// broadcast transitions across processes.
pub fn router(machines: HashMap<String, Arc<Machine>>) -> Router {
    Foster::new(machines).router()
}

/// Build the Foster API router with explicit store and pubsub backends.
///
/// Enables dependency injection of custom [`StateStore`] and [`PubSub`] backends.
/// The canonical use case is a Redis-backed implementation for HA / multi-replica
/// deployments where every server replica must share state and broadcast transitions.
///
/// The `version` field on [`Snapshot`] is the optimistic lock token: a Redis Lua
/// CAS script or `WATCH`/`MULTI` transaction is the typical way to implement atomic
/// [`StateStore::apply`] in a distributed setting.
///
/// For single-process deployments, prefer [`router`] which uses in-memory defaults.
///
/// Session isolation
/// ─────────────────
/// Every request carries a `session` query param (or `"default"` if absent).
/// Instances are keyed by `(session_id, machine_id)` and created lazily on first
/// access.  The WASM client stamps `data-fx-session` on `[fx-machine]` roots so
/// Playwright can discover the session and inject state without a page reload.
///
/// Wire protocol
/// ─────────────
///   GET  /state?machine=<id>&session=<sid>  → MessagePack Snapshot
///   POST /transition                         → MessagePack TransitionRequest → MessagePack Snapshot
///   GET  /events?machine=<id>&session=<sid> → SSE stream of JSON Snapshots
///   POST /test/state?session=<sid>           → JSON Snapshot in/out  (debug only)
pub fn router_with<S, P>(
    machines: HashMap<String, Arc<Machine>>,
    store: S,
    pubsub: P,
) -> Router
where
    S: StateStore + Clone + 'static,
    P: PubSub + Clone + 'static,
{
    Foster::new(machines).router_with(store, pubsub)
}

fn build_router<S, P>(foster: Foster, store: S, pubsub: P) -> Router
where
    S: StateStore + Clone + 'static,
    P: PubSub + Clone + 'static,
{
    let Foster { machines, feeds, request_events, effects, identity } = foster;
    // Collect template HTML and machine metadata before machines are moved into AppState.
    // Only one machine needs `.template(...)` set — the whole page is shared, and each
    // machine below validates only its own `[fx-machine="{id}"]` subtree(s) within it.
    let index_html = machines.values().find_map(|m| m.template.clone());

    // Validate every machine's own subtree(s) of the shared page — unknown fx-show
    // states or fx-on events panic immediately rather than silently misbehaving at
    // runtime. Scoped per-machine so multiple distinct machines can share one page
    // without each one's vocabulary being checked against the others' markup.
    if let Some(html) = &index_html {
        for (id, machine) in &machines {
            if let Err(errors) = machine.validate_in(html) {
                panic!(
                    "Machine '{}' template validation failed:\n{}",
                    id,
                    errors.join("\n")
                );
            }
        }
    }

    // `.local()` machines run in the browser: embed their definitions in the
    // page for the WASM client, and keep them out of the HTTP/SSE routes so a
    // per-visitor machine can't be driven (or read) through the server.
    // `type="application/json"` is never executed, so no CSP exemption is needed;
    // `</` is escaped so a context string can't close the tag early.
    let local_defs: Vec<_> = machines.values().filter_map(|m| m.local_def()).collect();
    let local_script = (!local_defs.is_empty()).then(|| {
        let json = serde_json::to_string(&local_defs)
            .expect("LocalMachineDef serializes")
            .replace("</", "<\\/");
        format!("<script type=\"application/json\" id=\"fx-local-machines\">{json}</script>")
    });
    let machines: HashMap<String, Arc<Machine>> =
        machines.into_iter().filter(|(_, m)| !m.local).collect();

    let test_mode = cfg!(debug_assertions)
        || std::env::var("FOSTER_TEST_MODE").map(|v| v == "1").unwrap_or(false);

    // Inject overlay CSS + machine metadata in debug builds.
    // CSS is served here (not by WASM) so it's in the document before WASM runs,
    // avoiding a render cycle where the panel briefly appears as an unstyled block.
    let overlay_script = if test_mode {
        serde_json::to_string(
            &machines.iter()
                .map(|(id, m)| (id.clone(), m.state_names()))
                .collect::<HashMap<_, _>>()
        )
        .ok()
        .map(|meta| format!(
            "<style id=\"fx-dbg-css\">{OVERLAY_CSS}</style>\
             <script>window.__FOSTER_MACHINES={meta};</script>"
        ))
    } else { None };

    for feed in &feeds {
        let m = machines.get(&feed.machine).unwrap_or_else(|| {
            panic!("feed: machine '{}' not found (or it's .local())", feed.machine)
        });
        assert!(m.shared, "feed: machine '{}' must be .shared()", feed.machine);
        assert!(
            m.transitions().iter().any(|(_, ev, _)| *ev == feed.event),
            "feed: machine '{}' has no '{}' transition", feed.machine, feed.event,
        );
    }
    for (id, events) in &effects {
        let m = machines.get(id).unwrap_or_else(|| panic!("effect: machine '{id}' not found (or it's .local())"));
        for event in events.keys() {
            assert!(
                m.transitions().iter().any(|(_, ev, _)| ev == event),
                "effect: machine '{id}' has no '{event}' transition",
            );
        }
    }
    for (id, re) in &request_events {
        let m = machines.get(id).unwrap_or_else(|| panic!("request_event: machine '{id}' not found"));
        assert!(
            m.transitions().iter().any(|(_, ev, _)| *ev == re.event),
            "request_event: machine '{id}' has no '{}' transition", re.event,
        );
    }

    let watchers = machines
        .iter()
        .filter(|(_, m)| m.shared)
        .map(|(id, _)| (id.clone(), watch::channel(0usize).0))
        .collect();

    let app = AppState {
        machines: Arc::new(machines),
        store,
        pubsub,
        test_mode,
        watchers: Arc::new(watchers),
        request_events: Arc::new(request_events),
        effects: Arc::new(effects),
        scoped: identity.is_some(),
    };

    for feed in feeds {
        spawn_feed(app.clone(), feed);
    }

    let mut router = Router::new()
        .route("/state", get(get_state::<S, P>))
        .route("/transition", post(post_transition::<S, P>))
        .route("/events", get(get_events::<S, P>))
        .route("/test/state", post(post_test_state::<S, P>))
        .route("/debug/history", get(get_debug_history::<S, P>))
        .route("/debug/rewind", post(post_debug_rewind::<S, P>))
        .route("/debug/graph", get(get_debug_graph::<S, P>))
        .route("/debug/timeline",   get(get_debug_timeline::<S, P>))
        .route("/debug/benchmark",  get(get_debug_benchmark::<S, P>))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .with_state(app);

    // Serve template at GET /, injecting the dev overlay in debug builds.
    if let Some(html) = index_html {
        let injected: String = [local_script, overlay_script].into_iter().flatten().collect();
        let served = if injected.is_empty() {
            html
        } else {
            // Last `</body>`: the real one, not one inside an inline script/string.
            match html.rfind("</body>") {
                Some(i) => format!("{}{injected}{}", &html[..i], &html[i..]),
                None => html + &injected,
            }
        };
        router = router.route("/", get(move || async move { Html(served) }));
    }

    match identity {
        Some(identify) => router.layer(middleware::from_fn(move |req: Request, next: Next| {
            let identify = identify.clone();
            async move { require_identity(identify, req, next).await }
        })),
        None => router,
    }
}

/// [`Foster::identity`]'s gate: `401` without a user, else the user goes into
/// the request's extensions for the handlers to scope sessions by.
async fn require_identity(identify: IdentityFn, mut req: Request, next: Next) -> Response {
    let info = RequestInfo {
        headers: req.headers().clone(),
        remote_addr: req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0),
        identity: None,
    };
    match identify(&info) {
        Some(id) => {
            req.extensions_mut().insert(Identity(id));
            next.run(req).await
        }
        None => (StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
    }
}

// ── wire helpers ──────────────────────────────────────────────────────────────

fn msgpack(data: &impl serde::Serialize) -> Response {
    match rmp_serde::to_vec_named(data) {
        Ok(bytes) => Response::builder()
            .header(header::CONTENT_TYPE, "application/msgpack")
            .body(Body::from(bytes))
            .unwrap(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn msgpack_err(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, msg.into()).into_response()
}

// ── query / request types ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct MachineQuery {
    machine: String,
    #[serde(default)]
    session: String,
}

impl MachineQuery {
    fn session_id(&self) -> &str {
        if self.session.is_empty() { "default" } else { &self.session }
    }
}

#[derive(Deserialize)]
struct SessionQuery {
    #[serde(default)]
    session: String,
}

impl SessionQuery {
    fn session_id(&self) -> &str {
        if self.session.is_empty() { "default" } else { &self.session }
    }
}

#[derive(serde::Serialize, Deserialize)]
struct TransitionRequest {
    machine: String,
    event: String,
    #[serde(default)]
    payload: Value,
    /// Session ID — set by the WASM client from the URL's `?session=` param.
    #[serde(default)]
    session: String,
}

// ── handlers ──────────────────────────────────────────────────────────────────

async fn get_state<S, P>(
    Query(q): Query<MachineQuery>,
    State(app): State<AppState<S, P>>,
    who: Option<Extension<Identity>>,
    headers: HeaderMap,
    // axum 0.8 has no `Option<ConnectInfo>` extractor; it's an extension.
    connect: Option<Extension<ConnectInfo<SocketAddr>>>,
) -> Response
where
    S: StateStore + Clone,
    P: PubSub + Clone,
{
    let Some(machine) = app.machines.get(&q.machine).cloned() else {
        return msgpack_err(StatusCode::NOT_FOUND, format!("machine '{}' not found", q.machine));
    };
    let who = who.map(|Extension(w)| w);
    let session = app.session(&q.machine, q.session_id(), who.as_ref());
    let session = session.as_str();

    // Request events refresh on every load — see `Foster::request_event`.
    if let Some(re) = app.request_events.get(&q.machine).cloned() {
        let identity = who.map(|Identity(id)| id);
        let payload = (re.handler)(RequestInfo { headers, remote_addr: connect.map(|Extension(ConnectInfo(a))| a), identity }).await;
        return match app.fire(session, &q.machine, &re.event, payload).await {
            Ok(snap) => msgpack(&snap),
            Err(e) => msgpack_err(StatusCode::BAD_REQUEST, e),
        };
    }

    let snap = match app.store.load(session, &q.machine).await {
        Some(s) => s,
        None => MachineInstance::new(machine).snapshot(),
    };
    msgpack(&snap)
}

async fn post_transition<S, P>(
    State(app): State<AppState<S, P>>,
    who: Option<Extension<Identity>>,
    headers: HeaderMap,
    // axum 0.8 has no `Option<ConnectInfo>` extractor; it's an extension.
    connect: Option<Extension<ConnectInfo<SocketAddr>>>,
    body: Bytes,
) -> Response
where
    S: StateStore + Clone,
    P: PubSub + Clone,
{
    let req: TransitionRequest = match rmp_serde::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return msgpack_err(StatusCode::BAD_REQUEST, e.to_string()),
    };

    let requested = if req.session.is_empty() { "default" } else { req.session.as_str() };
    if !app.machines.contains_key(&req.machine) {
        return msgpack_err(StatusCode::NOT_FOUND, format!("machine '{}' not found", req.machine));
    }
    let who = who.map(|Extension(w)| w);
    let session = app.session(&req.machine, requested, who.as_ref());

    let mut payload = req.payload;
    if let Some(re) = app.request_events.get(&req.machine).filter(|re| re.event == req.event).cloned() {
        let identity = who.map(|Identity(id)| id);
        let extra = (re.handler)(RequestInfo { headers, remote_addr: connect.map(|Extension(ConnectInfo(a))| a), identity }).await;
        payload = merge_shallow(payload, extra);
    }

    match app.fire(&session, &req.machine, &req.event, payload).await {
        Ok(snap) => msgpack(&snap),
        Err(e) => msgpack_err(StatusCode::BAD_REQUEST, e),
    }
}

/// Patch event sent over SSE after the first snapshot — only the diff is sent.
#[derive(serde::Serialize)]
struct ContextPatch {
    machine_id: String,
    state: String,
    version: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_event: Option<String>,
    patch: json_patch::Patch,
}

/// `GET /events` query: either one `machine` (+ `session`), or `subs` — a
/// comma-separated list of `machine@session` pairs multiplexed onto one
/// stream. Browsers cap HTTP/1.1 at 6 connections per host, so one SSE stream
/// per machine starves every other request once a page has a handful of
/// machines; the client sends all of its subscriptions here instead.
#[derive(Deserialize)]
struct EventsQuery {
    machine: Option<String>,
    #[serde(default)]
    session: String,
    subs: Option<String>,
}

/// Server-Sent Events stream for one or more `(machine, session)` pairs.
///
/// Per subscription, the first event is a named `snapshot` (full state) and
/// later ones are `patch` (RFC 6902 JSON Patch of just the context), which
/// keeps large contexts cheap. Every event's JSON carries `"sub"` — the
/// `machine@session` it belongs to, as requested — so a multiplexed client can
/// route it.
async fn get_events<S, P>(
    Query(q): Query<EventsQuery>,
    State(app): State<AppState<S, P>>,
    who: Option<Extension<Identity>>,
) -> Response
where
    S: StateStore + Clone,
    P: PubSub + Clone,
{
    let requested: Vec<(String, String)> = match (&q.subs, &q.machine) {
        (Some(subs), _) => subs
            .split(',')
            .filter_map(|sub| {
                let (m, sid) = sub.split_once('@')?;
                Some((m.to_string(), if sid.is_empty() { "default".into() } else { sid.to_string() }))
            })
            .collect(),
        (None, Some(m)) => {
            let sid = if q.session.is_empty() { "default".to_string() } else { q.session.clone() };
            vec![(m.clone(), sid)]
        }
        (None, None) => return (StatusCode::BAD_REQUEST, "machine or subs required").into_response(),
    };

    let mut guards = Vec::new();
    let mut streams = Vec::new();
    for (machine, sid) in requested {
        if !app.machines.contains_key(&machine) {
            continue;
        }
        let sub = format!("{machine}@{sid}");
        let session = app.session(&machine, &sid, who.as_ref().map(|Extension(w)| w));
        // Count this stream as a watcher of a shared machine (wakes its
        // feed) for as long as it's open: the guards drop with the stream.
        if let Some(tx) = app.watchers.get(&machine) {
            tx.send_modify(|n| *n += 1);
            guards.push(WatchGuard(tx.clone()));
        }
        // Subscribe, *then* read the stored snapshot and send it first: an
        // update published before this stream connected (an effect finishing
        // while the page loads, a reconnect after a network blip) isn't lost,
        // and nothing can slip between the read and the subscription. The
        // client drops anything not newer than what it has.
        let live = app.pubsub.subscribe(&session, &machine);
        let current = app.store.load(&session, &machine).await;
        streams.push(
            futures_util::stream::iter(current)
                .chain(live)
                .map(move |snap| (sub.clone(), snap))
                .boxed(),
        );
    }

    let mut prev: HashMap<String, Snapshot> = HashMap::new();
    let sse_stream = futures_util::stream::select_all(streams).map(move |(sub, snap)| {
        let _keep_alive = &guards;
        let (name, mut data) = match prev.get(&sub) {
            None => ("snapshot", serde_json::to_value(&snap).unwrap_or_default()),
            Some(p) => {
                let msg = ContextPatch {
                    machine_id: snap.machine_id.clone(),
                    state: snap.state.clone(),
                    version: snap.version,
                    last_event: snap.last_event.clone(),
                    patch: json_patch::diff(&p.context, &snap.context),
                };
                ("patch", serde_json::to_value(&msg).unwrap_or_default())
            }
        };
        data["sub"] = Value::String(sub.clone());
        prev.insert(sub, snap);
        Event::default()
            .event(name)
            .json_data(&data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    });
    Sse::new(sse_stream).keep_alive(KeepAlive::default()).into_response()
}

/// Debug: return the snapshot history ring buffer for a `(session, machine)` pair.
///
/// Returns oldest-first, capped at 50 entries.  Gated by `test_mode`.
async fn get_debug_history<S, P>(
    Query(q): Query<MachineQuery>,
    State(app): State<AppState<S, P>>,
    who: Option<Extension<Identity>>,
) -> Response
where
    S: StateStore + Clone,
    P: PubSub + Clone,
{
    if !app.test_mode {
        return (StatusCode::FORBIDDEN, "debug endpoints are disabled in production; set FOSTER_TEST_MODE=1 to enable").into_response();
    }
    let history = app.store.history(&app.session(&q.machine, q.session_id(), who.as_ref().map(|Extension(w)| w)), &q.machine).await;
    Json(history).into_response()
}

#[derive(Deserialize)]
struct RewindQuery {
    machine: String,
    version: u64,
    #[serde(default)]
    session: String,
}

impl RewindQuery {
    fn session_id(&self) -> &str {
        if self.session.is_empty() { "default" } else { &self.session }
    }
}

/// Rewind a machine instance to a previously recorded snapshot version.
///
/// Looks up `version` in the history ring buffer, validates state and schema via
/// `restore()`, then writes a new snapshot (at `current_version + 1`) and
/// publishes it over SSE so connected clients update immediately.
///
/// Gated by `test_mode`.
async fn post_debug_rewind<S, P>(
    Query(q): Query<RewindQuery>,
    State(app): State<AppState<S, P>>,
    who: Option<Extension<Identity>>,
) -> Result<Json<Snapshot>, (StatusCode, String)>
where
    S: StateStore + Clone,
    P: PubSub + Clone,
{
    if !app.test_mode {
        return Err((StatusCode::FORBIDDEN, "debug endpoints are disabled in production; set FOSTER_TEST_MODE=1 to enable".to_string()));
    }

    let session_id = app.session(&q.machine, q.session_id(), who.as_ref().map(|Extension(w)| w));

    let Some(machine) = app.machines.get(&q.machine).cloned() else {
        return Err((StatusCode::NOT_FOUND, format!("machine '{}' not found", q.machine)));
    };

    let history = app.store.history(&session_id, &q.machine).await;
    let target = history
        .iter()
        .find(|s| s.version == q.version)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("version {} not in history for machine '{}'", q.version, q.machine)))?
        .clone();

    // Validate state and schema via restore() on a throwaway instance.
    let mut validator = MachineInstance::new(Arc::clone(&machine));
    validator
        .restore(Snapshot { version: 0, ..target.clone() })
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    // Build the rewound snapshot at current_version+1 so the CAS check passes.
    let current_version = app.store.load(&session_id, &q.machine).await.map(|s| s.version).unwrap_or(0);
    let rewound = Snapshot {
        machine_id: q.machine.clone(),
        state: target.state,
        context: target.context,
        version: current_version + 1,
        last_event: None,
    };

    app.store
        .store(&session_id, &q.machine, &rewound)
        .await
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
    app.pubsub.publish(&session_id, &q.machine, rewound.clone()).await;

    Ok(Json(rewound))
}

/// State graph visualiser — returns a self-contained HTML page with an SVG diagram.
///
/// The diagram shows every state (node) and every event (directed edge).  The initial
/// state is drawn with a dashed outer ring.  An SSE subscription highlights the current
/// state in green without a page reload.
///
/// Gated by `test_mode` (debug builds or `FOSTER_TEST_MODE=1`).
async fn get_debug_graph<S, P>(
    Query(q): Query<MachineQuery>,
    State(app): State<AppState<S, P>>,
) -> Response
where
    S: StateStore + Clone,
    P: PubSub + Clone,
{
    if !app.test_mode {
        return (
            StatusCode::FORBIDDEN,
            "debug endpoints are disabled in production; set FOSTER_TEST_MODE=1 to enable",
        )
            .into_response();
    }
    let Some(machine) = app.machines.get(&q.machine).cloned() else {
        return (StatusCode::NOT_FOUND, format!("machine '{}' not found", q.machine))
            .into_response();
    };
    Html(build_graph_html(&machine, q.session_id())).into_response()
}

/// Build the self-contained HTML page for a given machine's state graph.
///
/// Graph data (nodes, edges, initial state) is serialised to JSON and injected as a
/// `<script>` block that the static renderer picks up.  All rendering logic lives in
/// `GRAPH_RENDER_JS` so it never passes through `format!` and needs no brace-escaping.
fn build_graph_html(machine: &Machine, session: &str) -> String {
    let machine_id = &machine.id;
    let initial_state = &machine.initial_state;

    let nodes_json = serde_json::json!(machine.state_names()).to_string();
    let edges_json = serde_json::to_string(
        &machine
            .transitions()
            .iter()
            .map(|(f, e, t)| serde_json::json!({"from": f, "event": e, "to": t}))
            .collect::<Vec<_>>(),
    )
    .unwrap_or_default();
    let initial_json = serde_json::json!(initial_state).to_string();
    let machine_id_json = serde_json::json!(machine_id).to_string();
    let session_json = serde_json::json!(session).to_string();

    // Note: use r##"..."## so that "# sequences inside the HTML (e.g. fill="#555")
    // do not accidentally terminate the raw-string literal.
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>State Graph — {machine_id}</title>
<style>{GRAPH_CSS}</style>
</head>
<body>
<h1>State Graph — {machine_id}</h1>
<p class="meta">machine: <code>{machine_id}</code> &nbsp;|&nbsp; session: <code>{session}</code></p>
<div class="conn-dot" id="conn-dot" title="SSE connection"></div>
<svg id="graph" viewBox="0 0 800 600" xmlns="http://www.w3.org/2000/svg">
{GRAPH_SVG_DEFS}
</svg>
<script>
const NODES = {nodes_json};
const EDGES = {edges_json};
const INITIAL = {initial_json};
const MACHINE = {machine_id_json};
const SESSION = {session_json};
</script>
<script>{GRAPH_RENDER_JS}</script>
</body>
</html>"##
    )
}

/// History timeline — self-contained HTML page showing every recorded snapshot
/// as a scrubable timeline.  Clicking a step rewinds the machine to that version.
/// An auto-play button replays the sequence at configurable speed.
///
/// Gated by `test_mode`.
async fn get_debug_timeline<S: StateStore + Clone, P: PubSub + Clone>(
    Query(q): Query<MachineQuery>,
    State(app): State<AppState<S, P>>,
) -> Response {
    if !app.test_mode {
        return (
            StatusCode::FORBIDDEN,
            "debug endpoints are disabled in production; set FOSTER_TEST_MODE=1 to enable",
        )
            .into_response();
    }
    let Some(machine) = app.machines.get(&q.machine).cloned() else {
        return (StatusCode::NOT_FOUND, format!("machine '{}' not found", q.machine))
            .into_response();
    };
    Html(build_timeline_html(&machine, q.session_id())).into_response()
}

/// `GET /debug/benchmark?machine=<id>`
///
/// Walks the machine graph in-memory from the initial state, firing every
/// reachable transition in BFS order.  For each step records the full-snapshot
/// context size vs the RFC 6902 JSON Patch size relative to the previous
/// context, showing exactly how much differential SSE saves.  Returns JSON.
/// Gated by `test_mode`.
async fn get_debug_benchmark<S: StateStore + Clone, P: PubSub + Clone>(
    Query(q): Query<MachineQuery>,
    State(app): State<AppState<S, P>>,
) -> Response {
    if !app.test_mode {
        return (StatusCode::FORBIDDEN,
            "debug endpoints are disabled; set FOSTER_TEST_MODE=1").into_response();
    }
    let Some(machine) = app.machines.get(&q.machine).cloned() else {
        return (StatusCode::NOT_FOUND, format!("machine '{}' not found", q.machine))
            .into_response();
    };

    // Walk the machine graph in-memory: BFS over (state → events).
    // Each step fires the event on a fresh instance restored to that state's
    // context, so we get realistic context diffs without touching any real session.
    let inst = MachineInstance::new(Arc::clone(&machine));
    let initial_snap = inst.snapshot();
    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut prev_ctx = initial_snap.context.clone();
    let mut queue: std::collections::VecDeque<Snapshot> = std::collections::VecDeque::new();
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    queue.push_back(initial_snap);

    while let Some(snap) = queue.pop_front() {
        if !visited.insert(snap.state.clone()) { continue; }
        for (from, event, _to) in machine.transitions() {
            if from != snap.state { continue; }
            let mut step = MachineInstance::new(Arc::clone(&machine));
            if step.restore(snap.clone()).is_err() { continue; }
            let Ok(new_snap) = step.send(event, serde_json::Value::Null) else { continue };

            let full_bytes  = serde_json::to_vec(&new_snap.context).unwrap_or_default().len();
            let patch       = json_patch::diff(&prev_ctx, &new_snap.context);
            let patch_bytes = serde_json::to_vec(&patch).unwrap_or_default().len();
            let patch_pct   = if full_bytes > 0 { patch_bytes * 100 / full_bytes } else { 100 };

            rows.push(serde_json::json!({
                "from":        from,
                "event":       event,
                "to":          new_snap.state,
                "full_bytes":  full_bytes,
                "patch_bytes": patch_bytes,
                "patch_pct":   patch_pct,
            }));

            prev_ctx = new_snap.context.clone();
            queue.push_back(new_snap);
        }
    }

    let total_full:  usize = rows.iter().filter_map(|r| r["full_bytes"].as_u64()).map(|n| n as usize).sum();
    let total_patch: usize = rows.iter().filter_map(|r| r["patch_bytes"].as_u64()).map(|n| n as usize).sum();
    let overall_pct = if total_full > 0 { total_patch * 100 / total_full } else { 100 };

    axum::Json(serde_json::json!({
        "machine":            machine.id,
        "transitions_walked": rows.len(),
        "total_full_bytes":   total_full,
        "total_patch_bytes":  total_patch,
        "overall_patch_pct":  overall_pct,
        "note": "patch_pct = patch size as % of full snapshot (lower = more SSE savings)",
        "steps": rows,
    })).into_response()
}

fn build_timeline_html(machine: &Machine, session: &str) -> String {
    let machine_id      = &machine.id;
    let machine_json    = serde_json::json!(machine_id).to_string();
    let session_json    = serde_json::json!(session).to_string();
    let preview_session = format!("{session}__tl");
    let preview_json    = serde_json::json!(preview_session).to_string();
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Timeline — {machine_id}</title>
<style>{TIMELINE_CSS}</style>
</head>
<body>
<div class="tl-header">
  <span class="tl-title">Timeline — <code>{machine_id}</code></span>
  <span class="tl-meta">session: <code>{session}</code></span>
  <span class="conn-dot" id="dot"></span>
</div>
<div class="tl-toolbar">
  <button id="btn-prev"  title="Previous step">◀</button>
  <button id="btn-play"  title="Play / Pause">▶ play</button>
  <button id="btn-next"  title="Next step">▶</button>
  <button id="btn-live"  title="Jump to latest">⏭ live</button>
  <label class="speed-label">speed
    <select id="speed">
      <option value="1200">0.5×</option>
      <option value="800" selected>1×</option>
      <option value="400">2×</option>
      <option value="200">4×</option>
    </select>
  </label>
  <span class="tl-count" id="count"></span>
</div>
<div class="tl-rail-wrap">
  <div class="tl-rail" id="rail"></div>
</div>
<div class="tl-body">
  <div class="tl-preview-wrap">
    <div class="tl-panel-label">ui preview</div>
    <iframe class="tl-preview" id="preview" src="/?session={preview_session}"></iframe>
  </div>
  <div class="tl-ctx-wrap">
    <div class="tl-panel-label">context</div>
    <pre class="tl-ctx" id="ctx">—</pre>
  </div>
</div>
<script>
const MACHINE         = {machine_json};
const SESSION         = {session_json};
const PREVIEW_SESSION = {preview_json};
</script>
<script>{TIMELINE_JS}</script>
</body>
</html>"##
    )
}

const TIMELINE_CSS: &str = r#"
* { box-sizing: border-box; margin: 0; padding: 0; }
body { font-family: monospace; background: #111; color: #ccc; display: flex;
       flex-direction: column; height: 100vh; overflow: hidden; }
code { background: #222; padding: 1px 5px; border-radius: 3px; font-size: 0.85em; }
.tl-header { display: flex; align-items: center; gap: 12px; padding: 10px 16px;
             background: #1a1a1a; border-bottom: 1px solid #2a2a2a; flex-shrink: 0; }
.tl-title  { font-size: 0.95rem; color: #aaa; }
.tl-meta   { font-size: 0.8rem; color: #555; }
.conn-dot  { width: 8px; height: 8px; border-radius: 50%; background: #555; margin-left: auto; }
.conn-dot.live { background: #4caf50; }
.tl-toolbar { display: flex; align-items: center; gap: 8px; padding: 8px 16px;
              background: #161616; border-bottom: 1px solid #222; flex-shrink: 0; }
.tl-toolbar button { font-family: monospace; background: #222; border: 1px solid #333;
                     color: #aaa; padding: 3px 10px; cursor: pointer; border-radius: 3px;
                     font-size: 0.82rem; }
.tl-toolbar button:hover { background: #2a2a2a; color: #ddd; }
.tl-toolbar button.playing { background: #1a3a1a; border-color: #2d6a2d; color: #4caf50; }
#btn-live { margin-left: 4px; }
.speed-label { font-size: 0.78rem; color: #555; margin-left: 8px; }
.speed-label select { font-family: monospace; background: #1a1a1a; border: 1px solid #2a2a2a;
                      color: #888; border-radius: 3px; padding: 2px 4px; font-size: 0.78rem; }
.tl-count  { font-size: 0.78rem; color: #555; margin-left: auto; }
.tl-rail-wrap { height: 130px; flex-shrink: 0; overflow-x: auto; overflow-y: hidden;
                padding: 16px 16px; border-bottom: 1px solid #1e1e1e; }
.tl-rail { display: flex; align-items: center; min-width: max-content; gap: 0; height: 100%; }
.tl-snap { display: flex; flex-direction: column; align-items: center; gap: 6px;
           padding: 10px 14px; border: 1px solid #2a2a2a; border-radius: 6px;
           background: #1a1a1a; cursor: pointer; min-width: 100px; max-width: 140px;
           transition: border-color 0.12s, background 0.12s; }
.tl-snap:hover { border-color: #444; background: #1e1e1e; }
.tl-snap.active { border-color: #4caf50; background: #0d2010; }
.tl-snap-ver   { font-size: 0.68rem; color: #555; }
.tl-snap-state { font-size: 0.85rem; color: #ddd; font-weight: 600; text-align: center;
                 word-break: break-all; }
.tl-snap.active .tl-snap-state { color: #4caf50; }
.tl-snap-ev    { font-size: 0.7rem; color: #666; text-align: center; word-break: break-all; }
.tl-connector  { width: 32px; flex-shrink: 0; height: 2px; background: #2a2a2a;
                 position: relative; }
.tl-connector::after { content: '▶'; position: absolute; right: -6px; top: -7px;
                       font-size: 10px; color: #333; }
/* ── split body: iframe preview + context JSON ─────────────────────────────── */
.tl-body { display: flex; flex: 1 1 auto; min-height: 0; overflow: hidden; }
.tl-preview-wrap { flex: 1 1 auto; display: flex; flex-direction: column; min-width: 0;
                   border-right: 1px solid #2a2a2a; }
.tl-preview { flex: 1; border: none; width: 100%; background: #fff; }
.tl-ctx-wrap { flex: 0 0 320px; display: flex; flex-direction: column; overflow: hidden; }
.tl-ctx { flex: 1; font-family: monospace; font-size: 0.8rem; color: #888;
          white-space: pre-wrap; word-break: break-all; line-height: 1.5;
          overflow: auto; padding: 12px 16px; }
.tl-panel-label { font-size: 0.7rem; color: #555; text-transform: uppercase;
                  letter-spacing: 0.1em; padding: 6px 16px 5px;
                  border-bottom: 1px solid #1e1e1e; flex-shrink: 0; }
"#;

const TIMELINE_JS: &str = r#"
(function () {
  const HISTORY_URL = `/debug/history?machine=${encodeURIComponent(MACHINE)}&session=${encodeURIComponent(SESSION)}`;
  const REWIND_URL  = v => `/debug/rewind?machine=${encodeURIComponent(MACHINE)}&session=${encodeURIComponent(SESSION)}&version=${v}`;
  const SSE_URL     = `/events?machine=${encodeURIComponent(MACHINE)}&session=${encodeURIComponent(SESSION)}`;

  let history  = [];
  let activeVer = null;
  let playTimer = null;
  let ignoreVer = null; // version we just rewound to — suppress echo

  // ── data fetch ──────────────────────────────────────────────────────────────
  function loadHistory() {
    return fetch(HISTORY_URL).then(r => r.json()).then(snaps => {
      history = snaps;
      render();
      updateCount();
    });
  }

  // ── render timeline ──────────────────────────────────────────────────────────
  function render() {
    const rail = document.getElementById('rail');
    rail.innerHTML = '';
    history.forEach((snap, i) => {
      if (i > 0) {
        const line = document.createElement('div');
        line.className = 'tl-connector';
        rail.appendChild(line);
      }
      const el = document.createElement('div');
      el.className = 'tl-snap' + (snap.version === activeVer ? ' active' : '');
      el.dataset.version = snap.version;
      el.innerHTML =
        `<div class="tl-snap-ver">v${snap.version}</div>` +
        `<div class="tl-snap-state">${snap.state}</div>` +
        `<div class="tl-snap-ev">${snap.last_event || '—'}</div>`;
      el.addEventListener('click', () => rewindTo(snap.version));
      rail.appendChild(el);
    });
    if (activeVer !== null) scrollToActive();
  }

  function updateCount() {
    const idx = history.findIndex(s => s.version === activeVer);
    const label = idx >= 0
      ? `step ${idx + 1} / ${history.length}`
      : `${history.length} steps`;
    document.getElementById('count').textContent = label;
  }

  // ── preview injection ─────────────────────────────────────────────────────────
  // Injects a historical snapshot into the isolated preview session so the iframe
  // renders the UI exactly as it was at that point in time.
  function injectPreview(snap) {
    if (!snap) return;
    fetch(`/test/state?session=${encodeURIComponent(PREVIEW_SESSION)}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        machine_id: MACHINE,
        state:      snap.state,
        context:    snap.context,
        version:    snap.version,
      }),
    });
  }

  // ── activation ───────────────────────────────────────────────────────────────
  function activate(version) {
    activeVer = version;
    document.querySelectorAll('.tl-snap').forEach(el => {
      el.classList.toggle('active', +el.dataset.version === version);
    });
    const snap = history.find(s => s.version === version);
    document.getElementById('ctx').textContent = snap
      ? JSON.stringify(snap.context, null, 2)
      : '—';
    updateCount();
    scrollToActive();
    injectPreview(snap);
  }

  function scrollToActive() {
    const el = document.querySelector(`.tl-snap[data-version="${activeVer}"]`);
    if (el) el.scrollIntoView({ behavior: 'smooth', block: 'nearest', inline: 'center' });
  }

  // ── rewind ────────────────────────────────────────────────────────────────────
  function rewindTo(version) {
    ignoreVer = version;
    fetch(REWIND_URL(version), { method: 'POST' });
    activate(version);
  }

  // ── play controls ─────────────────────────────────────────────────────────────
  const btnPlay = document.getElementById('btn-play');

  function stopPlay() {
    if (!playTimer) return;
    clearInterval(playTimer);
    playTimer = null;
    btnPlay.textContent = '▶ play';
    btnPlay.classList.remove('playing');
  }

  function startPlay() {
    stopPlay();
    let idx = history.findIndex(s => s.version === activeVer);
    if (idx < 0 || idx >= history.length - 1) idx = -1; // restart from top
    const speed = +document.getElementById('speed').value;
    btnPlay.textContent = '⏸ pause';
    btnPlay.classList.add('playing');
    playTimer = setInterval(() => {
      idx++;
      if (idx >= history.length) { stopPlay(); return; }
      rewindTo(history[idx].version);
    }, speed);
  }

  btnPlay.addEventListener('click', () => playTimer ? stopPlay() : startPlay());

  document.getElementById('btn-prev').addEventListener('click', () => {
    stopPlay();
    const idx = history.findIndex(s => s.version === activeVer);
    if (idx > 0) rewindTo(history[idx - 1].version);
  });

  document.getElementById('btn-next').addEventListener('click', () => {
    stopPlay();
    const idx = history.findIndex(s => s.version === activeVer);
    if (idx >= 0 && idx < history.length - 1) rewindTo(history[idx + 1].version);
  });

  document.getElementById('btn-live').addEventListener('click', () => {
    stopPlay();
    loadHistory().then(() => {
      if (history.length) rewindTo(history[history.length - 1].version);
    });
  });

  // ── SSE — live tail ───────────────────────────────────────────────────────────
  const dot = document.getElementById('dot');
  const es  = new EventSource(SSE_URL);
  es.onopen  = () => dot.classList.add('live');
  es.onerror = () => dot.classList.remove('live');

  const onSseSnap = ev => {
    let snap;
    try { snap = JSON.parse(ev.data); } catch (_) { return; }
    // Rewind echo: server bumps version, so ignore until next live event.
    if (snap.version === ignoreVer) { ignoreVer = null; return; }
    ignoreVer = null;
    if (!history.find(s => s.version === snap.version)) {
      history.push(snap);
      render();
    }
    activate(snap.version);
  };

  // patch events don't carry full context — refetch history then activate latest
  const onSsePatch = () => {
    if (ignoreVer !== null) { ignoreVer = null; return; }
    loadHistory().then(() => {
      if (history.length) activate(history[history.length - 1].version);
    });
  };

  es.addEventListener('snapshot', onSseSnap);
  es.addEventListener('patch',    onSsePatch);

  // ── init ──────────────────────────────────────────────────────────────────────
  loadHistory().then(() => {
    if (history.length) activate(history[history.length - 1].version);
  });
}());
"#;

// ── overlay CSS (served by the server so it's in the HTML before WASM runs) ───
const OVERLAY_CSS: &str = "\
.fx-dbg{position:fixed;bottom:14px;right:14px;z-index:2147483647;font-family:monospace;font-size:12px;background:#1a1a1a;border:1px solid #333;border-radius:8px;min-width:210px;box-shadow:0 4px 24px rgba(0,0,0,.6);color:#ccc}\
.fx-dbg.min .fx-dbg-body{display:none}\
.fx-dbg-head{display:flex;align-items:center;padding:7px 10px;gap:6px;background:#242424;border-radius:7px 7px 0 0}\
.fx-dbg-body{display:flex;flex-direction:column;padding:8px 10px;gap:5px}\
.fx-dbg-row{display:flex;justify-content:space-between;align-items:center}\
.fx-dbg-key{color:#666}\
.fx-dbg-st{display:inline-block;padding:1px 8px;border-radius:10px;background:#1a3a1a;color:#4caf50}\
.fx-dbg-jump{display:flex;gap:4px;margin-top:2px}\
.fx-dbg-jump select{flex:1;background:#111;color:#ccc;border:1px solid #333;border-radius:4px;padding:2px 4px;font:11px monospace}\
.fx-dbg-jump button{background:#2a4a2a;color:#4caf50;border:1px solid #2d6a2d;border-radius:4px;padding:2px 8px;cursor:pointer;font-size:11px}\
.fx-dbg-links{display:flex;gap:8px;margin-top:4px}\
.fx-dbg-links a{color:#4a9eff;text-decoration:none;font-size:11px}\
.fx-dbg-ctrl{background:none;border:none;color:#666;cursor:pointer;padding:0 2px;font-size:13px}\
";

// r##"..."## is required: fill="#555" contains "# which would terminate r#"..."#.
const GRAPH_SVG_DEFS: &str = r##"<defs>
  <marker id="arrow" markerWidth="8" markerHeight="8" refX="7" refY="3" orient="auto" markerUnits="strokeWidth">
    <path d="M0,0 L0,6 L8,3 z" fill="#555"/>
  </marker>
</defs>"##;

const GRAPH_CSS: &str = r#"
body { font-family: system-ui, sans-serif; background: #111; color: #eee; margin: 0; padding: 16px; }
h1   { margin: 0 0 6px; font-size: 1.1rem; color: #aaa; font-weight: normal; }
.meta { font-size: 0.8rem; color: #666; margin-bottom: 14px; }
code { background: #222; padding: 2px 6px; border-radius: 3px; }
svg  { display: block; width: 100%; max-width: 900px; background: #1a1a1a;
       border-radius: 8px; border: 1px solid #333; }
.edge-path  { fill: none; stroke: #555; stroke-width: 1.5; }
.edge-label { font-size: 10px; fill: #888; text-anchor: middle; dominant-baseline: middle; }
.edge-bg    { fill: #1a1a1a; }
.state-label { font-size: 13px; fill: #eee; text-anchor: middle; dominant-baseline: middle;
               pointer-events: none; }
.node-ring  { fill: #242424; stroke-width: 2; }
.n-normal   { stroke: #444; }
.n-initial  { stroke: #4a9eff; }
.n-current  { stroke: #4caf50; fill: #1a3a1a; }
.conn-dot   { position: fixed; top: 16px; right: 16px; width: 8px; height: 8px;
              border-radius: 50%; background: #555; }
.conn-dot.live { background: #4caf50; }
"#;

/// Pure JS — no Rust format args, so braces need no escaping.
const GRAPH_RENDER_JS: &str = r#"
(function () {
  const svg = document.getElementById('graph');
  const W = 800, H = 600, CX = W / 2, CY = H / 2, NR = 34;

  function mkEl(tag, attrs) {
    const el = document.createElementNS('http://www.w3.org/2000/svg', tag);
    for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
    return el;
  }

  // ── layout ──────────────────────────────────────────────────────────────────
  const pos = {};
  if (NODES.length === 1) {
    pos[NODES[0]] = { x: CX, y: CY };
  } else {
    const R = Math.min(W, H) * 0.33;
    NODES.forEach((name, i) => {
      const angle = (2 * Math.PI * i / NODES.length) - Math.PI / 2;
      pos[name] = { x: CX + R * Math.cos(angle), y: CY + R * Math.sin(angle) };
    });
  }

  // ── group edges by (from, to) ────────────────────────────────────────────
  const SEP = '\x00';
  const groups = {};
  EDGES.forEach(e => {
    const key = e.from + SEP + e.to;
    if (!groups[key]) groups[key] = { from: e.from, to: e.to, events: [] };
    groups[key].events.push(e.event);
  });
  function hasBidi(a, b) { return !!(groups[b + SEP + a]); }

  // ── draw edges ───────────────────────────────────────────────────────────
  // Inserted before nodes so nodes appear on top.
  const edgeLayer = mkEl('g', {});
  svg.appendChild(edgeLayer);

  Object.values(groups).forEach(({ from, to, events }) => {
    const label = events.join(' / ');
    const fp = pos[from], tp = pos[to];
    let pathD, lx, ly;

    if (from === to) {
      // Self-loop: small arc above the node
      const sx = fp.x - NR * 0.7, sy = fp.y - NR;
      const ex = fp.x + NR * 0.7, ey = fp.y - NR;
      pathD = `M ${sx} ${sy} C ${sx} ${sy - 44} ${ex} ${ey - 44} ${ex} ${ey}`;
      lx = fp.x; ly = fp.y - NR - 32;
    } else {
      const dx = tp.x - fp.x, dy = tp.y - fp.y;
      const len = Math.sqrt(dx * dx + dy * dy) || 1;
      const ux = dx / len, uy = dy / len;
      // Shorten to node boundary
      const sx = fp.x + ux * NR, sy = fp.y + uy * NR;
      const ex = tp.x - ux * (NR + 6), ey = tp.y - uy * (NR + 6);

      if (hasBidi(from, to)) {
        // Curve to avoid overlap with the reverse edge
        const ox = -uy * 44, oy = ux * 44;
        const mx = (sx + ex) / 2 + ox, my = (sy + ey) / 2 + oy;
        pathD = `M ${sx} ${sy} Q ${mx} ${my} ${ex} ${ey}`;
        lx = mx; ly = my;
      } else {
        pathD = `M ${sx} ${sy} L ${ex} ${ey}`;
        lx = (sx + ex) / 2; ly = (sy + ey) / 2;
      }
    }

    const path = mkEl('path', { d: pathD, class: 'edge-path', 'marker-end': 'url(#arrow)' });
    edgeLayer.appendChild(path);

    // Label — small text with a background rect for legibility
    const charW = 5.5, pad = 4;
    const lw = label.length * charW + pad * 2, lh = 14;
    const bg = mkEl('rect', {
      x: lx - lw / 2, y: ly - lh / 2, width: lw, height: lh, rx: 2, class: 'edge-bg'
    });
    const txt = mkEl('text', { x: lx, y: ly, class: 'edge-label' });
    txt.textContent = label;
    edgeLayer.appendChild(bg);
    edgeLayer.appendChild(txt);
  });

  // ── draw nodes ───────────────────────────────────────────────────────────
  const circles = {};
  NODES.forEach(name => {
    const { x, y } = pos[name];
    const isInitial = name === INITIAL;
    const g = mkEl('g', {});

    if (isInitial) {
      // Dashed outer ring marks the initial state
      g.appendChild(mkEl('circle', {
        cx: x, cy: y, r: NR + 6,
        fill: 'none', stroke: '#4a9eff', 'stroke-width': 1, 'stroke-dasharray': '3,3'
      }));
    }

    const circle = mkEl('circle', { cx: x, cy: y, r: NR, class: 'node-ring n-' + (isInitial ? 'initial' : 'normal') });
    const text   = mkEl('text',   { x, y, class: 'state-label' });
    text.textContent = name;
    g.appendChild(circle);
    g.appendChild(text);
    svg.appendChild(g);
    circles[name] = circle;
  });

  // ── SSE — highlight current state ────────────────────────────────────────
  let currentState = null;
  function setCurrentState(state) {
    if (currentState === state) return;
    if (currentState && circles[currentState]) {
      const prev = currentState === INITIAL ? 'n-initial' : 'n-normal';
      circles[currentState].setAttribute('class', 'node-ring ' + prev);
    }
    currentState = state;
    if (state && circles[state]) {
      circles[state].setAttribute('class', 'node-ring n-current');
    }
  }

  const dot = document.getElementById('conn-dot');
  const url = `/events?machine=${encodeURIComponent(MACHINE)}&session=${encodeURIComponent(SESSION)}`;
  const es = new EventSource(url);
  es.onopen  = () => dot.classList.add('live');
  es.onerror = () => dot.classList.remove('live');
  const onState = ev => { try { setCurrentState(JSON.parse(ev.data).state); } catch (_) {} };
  es.addEventListener('snapshot', onState);
  es.addEventListener('patch',    onState);
}());
"#;

/// Inject an arbitrary snapshot — bypasses all transition logic.
///
/// Gated by `test_mode` (on in debug builds, off in release unless
/// `FOSTER_TEST_MODE=1`).  Returns 403 in production.
///
/// curl example:
///   curl -X POST 'http://localhost:3000/test/state?session=my-test' \
///        -H 'Content-Type: application/json' \
///        -d '{"machine_id":"counter","state":"error","context":{"count":99},"version":0}'
async fn post_test_state<S, P>(
    Query(q): Query<SessionQuery>,
    State(app): State<AppState<S, P>>,
    who: Option<Extension<Identity>>,
    Json(snap): Json<Snapshot>,
) -> Result<Json<Snapshot>, (StatusCode, String)>
where
    S: StateStore + Clone,
    P: PubSub + Clone,
{
    if !app.test_mode {
        return Err((StatusCode::FORBIDDEN, "test endpoints are disabled in production; set FOSTER_TEST_MODE=1 to enable".to_string()));
    }

    let machine_id = snap.machine_id.clone();
    let session_id = app.session(&machine_id, q.session_id(), who.as_ref().map(|Extension(w)| w));

    let Some(machine) = app.machines.get(&machine_id).cloned() else {
        return Err((StatusCode::NOT_FOUND, format!("machine '{machine_id}' not found")));
    };

    let mut inst = MachineInstance::new(machine);
    inst.restore(snap.clone()).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let restored = inst.snapshot();

    app.store.store(&session_id, &machine_id, &restored).await
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
    app.pubsub.publish(&session_id, &machine_id, restored.clone()).await;

    Ok(Json(restored))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use foster_core::MachineBuilder;
    use serde_json::json;

    fn two_state_machine() -> Arc<Machine> {
        MachineBuilder::new("counter", "idle", json!({ "count": 0 }))
            .state("error")
            .on("idle", "increment", "idle", |ctx, _| {
                Ok(json!({ "count": ctx["count"].as_i64().unwrap_or(0) + 1 }))
            })
            .pass("idle", "break_it", "error")
            .pass("error", "recover", "idle")
            .build()
    }

    #[test]
    fn graph_html_contains_machine_id() {
        let m = two_state_machine();
        let html = build_graph_html(&m, "default");
        assert!(html.contains("counter"), "machine id missing from graph html");
    }

    #[test]
    fn graph_html_contains_all_states_in_json() {
        let m = two_state_machine();
        let html = build_graph_html(&m, "default");
        assert!(html.contains("\"idle\""), "state 'idle' missing from graph html");
        assert!(html.contains("\"error\""), "state 'error' missing from graph html");
    }

    #[test]
    fn graph_html_contains_all_events_in_json() {
        let m = two_state_machine();
        let html = build_graph_html(&m, "default");
        assert!(html.contains("\"increment\""), "event 'increment' missing");
        assert!(html.contains("\"break_it\""), "event 'break_it' missing");
        assert!(html.contains("\"recover\""), "event 'recover' missing");
    }

    #[test]
    fn graph_html_marks_initial_state() {
        let m = two_state_machine();
        let html = build_graph_html(&m, "default");
        // INITIAL constant must appear as the JSON string "idle"
        assert!(html.contains(r#"const INITIAL = "idle""#), "INITIAL constant missing or wrong");
    }

    #[test]
    fn graph_html_embeds_session_and_machine_constants() {
        let m = two_state_machine();
        let html = build_graph_html(&m, "my-session");
        assert!(html.contains(r#"const SESSION = "my-session""#));
        assert!(html.contains(r#"const MACHINE = "counter""#));
    }

    #[test]
    fn graph_html_is_valid_document_shell() {
        let m = two_state_machine();
        let html = build_graph_html(&m, "default");
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<svg"));
        assert!(html.contains("</html>"));
    }

    #[test]
    fn graph_html_single_state_machine() {
        let m = MachineBuilder::new("alone", "only", json!({}))
            .pass("only", "noop", "only")
            .build();
        let html = build_graph_html(&m, "s");
        assert!(html.contains("\"only\""));
        assert!(html.contains("\"noop\""));
    }

    // ── HTTP integration tests ────────────────────────────────────────────────

    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_machines() -> HashMap<String, Arc<Machine>> {
        HashMap::from([("counter".to_string(), two_state_machine())])
    }

    #[tokio::test]
    async fn http_get_state_returns_initial_snapshot() {
        let app = router(test_machines());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/state?machine=counter")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let snap: Snapshot = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(snap.machine_id, "counter");
        assert_eq!(snap.state, "idle");
        assert_eq!(snap.version, 0);
        assert_eq!(snap.context["count"], 0);
    }

    fn with_local_theme() -> HashMap<String, Arc<Machine>> {
        let theme = MachineBuilder::new("theme", "light", json!({"note": "</script>"}))
            .pass("light", "toggle", "dark")
            .pass("dark", "toggle", "light")
            .persist()
            .template(
                r#"<html><body><div fx-machine="theme"><button fx-on="click->toggle">t</button></div></body></html>"#,
            )
            .build();
        let mut m = test_machines();
        m.insert("theme".to_string(), theme);
        m
    }

    async fn get(app: Router, uri: &str) -> (StatusCode, String) {
        let resp = app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    // ── shared machines, feeds, request events ─────────────────────────────────

    fn live() -> MachineBuilder {
        MachineBuilder::new("live", "waiting", json!({}))
            .merge("waiting", "tick", "live")
            .merge("live", "tick", "live")
    }

    async fn state_of(app: &Router, uri: &str) -> Snapshot {
        let resp = app.clone().oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        rmp_serde::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }

    async fn transition(app: &Router, machine: &str, event: &str, session: &str) -> Snapshot {
        let body = rmp_serde::to_vec_named(&json!({"machine": machine, "event": event, "payload": null, "session": session})).unwrap();
        let resp = app.clone()
            .oneshot(Request::builder().method("POST").uri("/transition").body(Body::from(body)).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        rmp_serde::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }

    /// Concurrent events on one instance (two effects reporting at once)
    /// must all land: a raced write is retried, not dropped.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_transitions_on_one_instance_all_apply() {
        let counter = MachineBuilder::new("counter", "idle", json!({ "n": 0 }))
            .on("idle", "inc", "idle", |mut ctx, _| {
                // Widen the window between the store's read and write.
                std::thread::sleep(std::time::Duration::from_millis(2));
                ctx["n"] = json!(ctx["n"].as_i64().unwrap_or(0) + 1);
                Ok(ctx)
            })
            .build();
        let app = router(HashMap::from([("counter".to_string(), counter)]));
        let sends = (0..12).map(|_| {
            let app = app.clone();
            tokio::spawn(async move { transition(&app, "counter", "inc", "s").await })
        });
        for t in futures_util::future::join_all(sends).await {
            t.unwrap();
        }
        assert_eq!(state_of(&app, "/state?machine=counter&session=s").await.context["n"], 12);
    }

    #[tokio::test]
    async fn shared_machine_is_one_instance_for_all_sessions() {
        let app = router(HashMap::from([("live".to_string(), live().shared().build())]));
        transition(&app, "live", "tick", "alice").await;
        assert_eq!(state_of(&app, "/state?machine=live&session=bob").await.state, "live");
    }

    #[tokio::test]
    async fn feed_runs_only_while_watched() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let stream = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|v| (v, rx)) });
        let app = Foster::new(HashMap::from([("live".to_string(), live().shared().build())]))
            .feed("live", "tick", stream)
            .router();

        tx.send(json!({"n": 1})).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Nobody watching: the feed hasn't pulled the item.
        assert_eq!(state_of(&app, "/state?machine=live").await.state, "waiting");

        // Open an /events stream (held by `_sse`) — the feed wakes up.
        let _sse = app.clone()
            .oneshot(Request::builder().uri("/events?machine=live&session=x").body(Body::empty()).unwrap())
            .await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let snap = state_of(&app, "/state?machine=live&session=anyone").await;
        assert_eq!((snap.state.as_str(), snap.context["n"].as_i64()), ("live", Some(1)));
    }

    #[tokio::test]
    async fn shared_versions_are_comparable_across_replicas() {
        // Two independent routers stand in for two replicas: one has seen
        // many more updates than the other, yet its versions must not leave
        // the other's (newer) updates looking stale.
        let busy = router(HashMap::from([("live".to_string(), live().shared().build())]));
        for _ in 0..50 {
            transition(&busy, "live", "tick", "a").await;
        }
        let busy_v = state_of(&busy, "/state?machine=live").await.version;
        // Each update adds at most 2 on top of the clock, so a burst can run a
        // few ms ahead of wall time; a real feed ticks far slower than that.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let quiet = router(HashMap::from([("live".to_string(), live().shared().build())]));
        let quiet_v = transition(&quiet, "live", "tick", "b").await.version;
        assert!(quiet_v > busy_v, "later update on another replica ({quiet_v}) must beat {busy_v}");
    }

    #[test]
    #[should_panic(expected = "must be .shared()")]
    fn feed_requires_shared_machine() {
        let _ = Foster::new(HashMap::from([("live".to_string(), live().build())]))
            .feed("live", "tick", futures_util::stream::empty())
            .router();
    }

    #[test]
    #[should_panic(expected = "has no 'nope' transition")]
    fn feed_requires_known_event() {
        let _ = Foster::new(HashMap::from([("live".to_string(), live().shared().build())]))
            .feed("live", "nope", futures_util::stream::empty())
            .router();
    }

    #[tokio::test]
    async fn events_multiplexes_subscriptions() {
        let machines = HashMap::from([
            ("counter".to_string(), two_state_machine()),
            ("live".to_string(), live().shared().build()),
        ]);
        let app = router(machines);
        let resp = app.clone()
            .oneshot(Request::builder().uri("/events?subs=counter@s1,live@s1,nope@s1").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let mut body = resp.into_body();

        transition(&app, "live", "tick", "someone-else").await; // shared: reaches s1 too
        transition(&app, "counter", "increment", "s1").await;

        let mut text = String::new();
        while !(text.contains(r#""sub":"live@s1""#) && text.contains(r#""sub":"counter@s1""#)) {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(2), body.frame())
                .await.expect("event within 2s").unwrap().unwrap();
            if let Some(data) = frame.data_ref() { text.push_str(&String::from_utf8_lossy(data)); }
        }
        assert!(text.contains("event: snapshot"));
    }

    fn traced() -> Router {
        let m = MachineBuilder::new("trace", "pending", json!({}))
            .merge("pending", "trace", "traced")
            .merge("traced", "trace", "traced")
            .build();
        Foster::new(HashMap::from([("trace".to_string(), m)]))
            .request_event("trace", "trace", |req: RequestInfo| async move {
                let ua = req.headers.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                json!({ "ua": ua })
            })
            .router()
    }

    #[tokio::test]
    async fn request_event_fires_on_every_state_load() {
        let app = traced();
        let req = || Request::builder().uri("/state?machine=trace&session=s").header("user-agent", "probe/1").body(Body::empty()).unwrap();
        let first: Snapshot = rmp_serde::from_slice(&app.clone().oneshot(req()).await.unwrap().into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!((first.state.as_str(), first.context["ua"].as_str()), ("traced", Some("probe/1")));
        let second: Snapshot = rmp_serde::from_slice(&app.clone().oneshot(req()).await.unwrap().into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert!(second.version > first.version, "fresh trace on each load");
    }

    #[tokio::test]
    async fn request_event_payload_merged_into_client_event() {
        let app = traced();
        let body = rmp_serde::to_vec_named(&json!({"machine": "trace", "event": "trace", "payload": {"keep": 1}, "session": "s"})).unwrap();
        let resp = app.clone()
            .oneshot(Request::builder().method("POST").uri("/transition").header("user-agent", "btn/2").body(Body::from(body)).unwrap())
            .await.unwrap();
        let snap: Snapshot = rmp_serde::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(snap.context, json!({"keep": 1, "ua": "btn/2"}));
    }

    // ── effects ────────────────────────────────────────────────────────────────

    fn dir() -> MachineBuilder {
        MachineBuilder::new("dir", "idle", json!({}))
            .merge("idle", "open", "loading")
            .merge("loading", "loaded", "ready")
    }

    async fn send(app: &Router, machine: &str, event: &str, session: &str, payload: Value) -> Snapshot {
        let body = rmp_serde::to_vec_named(&json!({"machine": machine, "event": event, "payload": payload, "session": session})).unwrap();
        let resp = app.clone()
            .oneshot(Request::builder().method("POST").uri("/transition").body(Body::from(body)).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        rmp_serde::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }

    /// Polls `/state` until `machine` reaches `state` (effects run on a spawned task).
    async fn eventually(app: &Router, machine: &str, session: &str, state: &str) -> Snapshot {
        let uri = format!("/state?machine={machine}&session={session}");
        for _ in 0..200 {
            let snap = state_of(app, &uri).await;
            if snap.state == state { return snap; }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("{machine}@{session} never reached '{state}'");
    }

    #[tokio::test]
    async fn effect_sends_result_into_triggering_session() {
        let app = Foster::new(HashMap::from([("dir".to_string(), dir().build())]))
            .effect("dir", "open", |fx: Effect| async move {
                assert_eq!(fx.snapshot.state, "loading");
                let path = fx.payload["path"].as_str().unwrap_or("").to_string();
                fx.send("loaded", json!({ "entries": [format!("{path}/a.txt")] })).await.unwrap();
            })
            .router();
        let snap = send(&app, "dir", "open", "alice", json!({ "path": "/docs" })).await;
        assert_eq!(snap.state, "loading", "the response is the triggering transition");
        let done = eventually(&app, "dir", "alice", "ready").await;
        assert_eq!(done.context["entries"], json!(["/docs/a.txt"]));
        // Other sessions are untouched.
        assert_eq!(state_of(&app, "/state?machine=dir&session=bob").await.state, "idle");
    }

    #[tokio::test]
    async fn effect_can_reach_shared_machine() {
        let app = Foster::new(HashMap::from([
            ("dir".to_string(), dir().build()),
            ("live".to_string(), live().shared().build()),
        ]))
        .effect("dir", "open", |fx: Effect| async move {
            fx.send_to("live", "tick", json!({ "opened_by": fx.session })).await.unwrap();
        })
        .router();
        send(&app, "dir", "open", "alice", json!({})).await;
        let live = eventually(&app, "live", "anyone", "live").await;
        assert_eq!(live.context["opened_by"], "alice");
    }

    #[test]
    #[should_panic(expected = "effect: machine 'dir' has no 'nope' transition")]
    fn effect_requires_known_event() {
        let _ = Foster::new(HashMap::from([("dir".to_string(), dir().build())]))
            .effect("dir", "nope", |_fx: Effect| async {})
            .router();
    }

    #[test]
    #[should_panic(expected = "effect: machine 'ghost' not found")]
    fn effect_requires_known_machine() {
        let _ = Foster::new(HashMap::from([("dir".to_string(), dir().build())]))
            .effect("ghost", "open", |_fx: Effect| async {})
            .router();
    }

    #[tokio::test]
    async fn events_start_with_the_current_snapshot() {
        // A result that lands before the page's stream connects isn't lost.
        let app = router(HashMap::from([("dir".to_string(), dir().build())]));
        send(&app, "dir", "open", "s", json!({ "path": "/x" })).await;
        let resp = app.clone()
            .oneshot(Request::builder().uri("/events?subs=dir@s").body(Body::empty()).unwrap())
            .await.unwrap();
        let mut body = resp.into_body();
        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), body.frame())
            .await.expect("snapshot without any new transition").unwrap().unwrap();
        let text = String::from_utf8_lossy(frame.data_ref().unwrap()).into_owned();
        assert!(text.contains("event: snapshot") && text.contains(r#""state":"loading""#), "{text}");
    }

    // ── identity ───────────────────────────────────────────────────────────────

    /// `x-user` header = the user; a template so `GET /` exists.
    fn authed(machines: HashMap<String, Arc<Machine>>) -> Foster {
        Foster::new(machines)
            .identity(|req: &RequestInfo| req.headers.get("x-user")?.to_str().ok().map(str::to_string))
    }

    fn page_dir() -> MachineBuilder {
        dir().template(r#"<div fx-machine="dir"><button fx-on="click->open">o</button></div>"#)
    }

    async fn send_as(app: &Router, user: &str, machine: &str, event: &str, session: &str, payload: Value) -> (StatusCode, Option<Snapshot>) {
        let body = rmp_serde::to_vec_named(&json!({"machine": machine, "event": event, "payload": payload, "session": session})).unwrap();
        let mut req = Request::builder().method("POST").uri("/transition");
        if !user.is_empty() { req = req.header("x-user", user); }
        let resp = app.clone().oneshot(req.body(Body::from(body)).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, (status == StatusCode::OK).then(|| rmp_serde::from_slice(&bytes).unwrap()))
    }

    async fn state_as(app: &Router, user: &str, uri: &str) -> Snapshot {
        let resp = app.clone()
            .oneshot(Request::builder().uri(uri).header("x-user", user).body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        rmp_serde::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }

    #[tokio::test]
    async fn identity_rejects_anonymous_requests() {
        let app = authed(HashMap::from([("dir".to_string(), page_dir().build())])).router();
        for uri in ["/", "/state?machine=dir&session=s", "/events?subs=dir@s"] {
            let (status, _) = get(app.clone(), uri).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        }
        let (status, _) = send_as(&app, "", "dir", "open", "s", json!({})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let resp = app.clone()
            .oneshot(Request::builder().uri("/").header("x-user", "alice").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn identity_scopes_sessions_per_user() {
        let app = authed(HashMap::from([("dir".to_string(), dir().build())])).router();
        // Same session ID, different users: separate instances.
        let (_, snap) = send_as(&app, "alice", "dir", "open", "shared-id", json!({ "path": "/a" })).await;
        assert_eq!(snap.unwrap().state, "loading");
        assert_eq!(state_as(&app, "mallory", "/state?machine=dir&session=shared-id").await.state, "idle");
        assert_eq!(state_as(&app, "alice", "/state?machine=dir&session=shared-id").await.state, "loading");
    }

    #[tokio::test]
    async fn identity_reaches_effects_and_their_results_stay_scoped() {
        let app = authed(HashMap::from([("dir".to_string(), dir().build())]))
            .effect("dir", "open", |fx: Effect| async move {
                assert_eq!(fx.session, "s1", "the client's own session ID");
                let who = fx.identity.clone().unwrap_or_default();
                fx.send("loaded", json!({ "by": who })).await.unwrap();
            })
            .router();
        send_as(&app, "alice", "dir", "open", "s1", json!({})).await;
        let uri = "/state?machine=dir&session=s1";
        for _ in 0..200 {
            if state_as(&app, "alice", uri).await.state == "ready" { break; }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let done = state_as(&app, "alice", uri).await;
        assert_eq!((done.state.as_str(), done.context["by"].as_str()), ("ready", Some("alice")));
        assert_eq!(state_as(&app, "bob", uri).await.state, "idle");
    }

    #[tokio::test]
    async fn identity_leaves_shared_machines_global() {
        let app = authed(HashMap::from([("live".to_string(), live().shared().build())])).router();
        send_as(&app, "alice", "live", "tick", "a", json!({ "n": 1 })).await;
        assert_eq!(state_as(&app, "bob", "/state?machine=live&session=b").await.state, "live");
    }

    #[test]
    fn scoped_keys_round_trip_and_cannot_collide() {
        let app = AppState {
            machines: Arc::new(HashMap::new()),
            store: InMemoryStore::default(),
            pubsub: InMemoryPubSub::default(),
            test_mode: false,
            watchers: Arc::new(HashMap::new()),
            request_events: Arc::new(HashMap::new()),
            effects: Arc::new(HashMap::new()),
            scoped: true,
        };
        let key = |id: &str, sid: &str| app.session("m", sid, Some(&Identity(id.into())));
        assert_ne!(key("a:b", "c"), key("a", "b:c"));
        assert_ne!(key("a", "1:x"), key("1:a", "x"));
        for (id, sid) in [("alice@example.com", "s1"), ("we:ird", "se:ss:ion"), ("", "")] {
            assert_eq!(app.split_key(&key(id, sid)), (Some(id), sid));
        }
    }

    #[tokio::test]
    async fn local_machine_is_embedded_in_page() {
        let (status, html) = get(router(with_local_theme()), "/").await;
        assert_eq!(status, StatusCode::OK);
        let start = html.find(r#"<script type="application/json" id="fx-local-machines">"#)
            .expect("local machine script injected");
        assert!(start < html.rfind("</body>").unwrap());
        let body = &html[start..];
        let json = &body[body.find('>').unwrap() + 1..body.find("</script>").unwrap()];
        // The context's "</script>" is escaped, so the tag isn't closed early…
        assert!(!json.contains("</"));
        // …and it still parses back to the original value.
        let defs: Vec<foster_core::LocalMachineDef> = serde_json::from_str(json).unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].id, "theme");
        assert!(defs[0].persist);
        assert_eq!(defs[0].initial_context["note"], "</script>");
    }

    #[tokio::test]
    async fn local_machine_has_no_server_routes() {
        let (status, _) = get(router(with_local_theme()), "/state?machine=theme").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // Server machines on the same page are unaffected.
        let (status, _) = get(router(with_local_theme()), "/state?machine=counter").await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn no_local_script_without_local_machines() {
        let m = MachineBuilder::new("counter", "idle", json!({}))
            .template("<html><body></body></html>")
            .build();
        let (_, html) = get(router(HashMap::from([("counter".to_string(), m)])), "/").await;
        assert!(!html.contains("fx-local-machines"));
    }

    #[tokio::test]
    async fn http_get_state_404_for_unknown_machine() {
        let app = router(test_machines());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/state?machine=unknown")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn http_post_transition_increments_counter() {
        let app = router(test_machines());
        let req_body = TransitionRequest {
            machine: "counter".to_string(),
            event: "increment".to_string(),
            payload: Value::Null,
            session: "t1".to_string(),
        };
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transition")
                    .body(Body::from(rmp_serde::to_vec_named(&req_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let snap: Snapshot = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(snap.state, "idle");
        assert_eq!(snap.context["count"], 1);
        assert_eq!(snap.version, 1);
        assert_eq!(snap.last_event.as_deref(), Some("increment"));
    }

    #[tokio::test]
    async fn http_post_transition_state_change() {
        let app = router(test_machines());
        let req_body = TransitionRequest {
            machine: "counter".to_string(),
            event: "break_it".to_string(),
            payload: Value::Null,
            session: "t2".to_string(),
        };
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transition")
                    .body(Body::from(rmp_serde::to_vec_named(&req_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let snap: Snapshot = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(snap.state, "error");
    }

    #[tokio::test]
    async fn http_post_transition_404_for_unknown_machine() {
        let app = router(test_machines());
        let req_body = TransitionRequest {
            machine: "nope".to_string(),
            event: "x".to_string(),
            payload: Value::Null,
            session: "t3".to_string(),
        };
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transition")
                    .body(Body::from(rmp_serde::to_vec_named(&req_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn http_post_transition_400_for_invalid_event() {
        let app = router(test_machines());
        let req_body = TransitionRequest {
            machine: "counter".to_string(),
            event: "no_such_event".to_string(),
            payload: Value::Null,
            session: "t4".to_string(),
        };
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transition")
                    .body(Body::from(rmp_serde::to_vec_named(&req_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn http_post_test_state_injects_snapshot() {
        let app = router(test_machines());
        let snap = Snapshot {
            machine_id: "counter".to_string(),
            state: "error".to_string(),
            context: json!({ "count": 99 }),
            version: 0,
            last_event: None,
        };
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/test/state?session=inject-test")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&snap).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let returned: Snapshot = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(returned.state, "error");
        assert_eq!(returned.context["count"], 99);
    }

    #[tokio::test]
    async fn http_debug_history_records_transitions() {
        let store = InMemoryStore::default();
        let pubsub = InMemoryPubSub::default();
        let app = router_with(test_machines(), store, pubsub);

        // Fire a transition
        let req_body = TransitionRequest {
            machine: "counter".to_string(),
            event: "increment".to_string(),
            payload: Value::Null,
            session: "hist-s".to_string(),
        };
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/transition")
                    .body(Body::from(rmp_serde::to_vec_named(&req_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Check history
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/debug/history?machine=counter&session=hist-s")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let history: Vec<Snapshot> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].state, "idle");
        assert_eq!(history[0].context["count"], 1);
    }

    #[tokio::test]
    async fn router_with_accepts_custom_backends() {
        let app = router_with(
            test_machines(),
            InMemoryStore::default(),
            InMemoryPubSub::default(),
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/state?machine=counter")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let snap: Snapshot = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(snap.machine_id, "counter");
        assert_eq!(snap.state, "idle");
    }

    #[tokio::test]
    async fn ha_shared_store_state_visible_across_replicas() {
        let store = InMemoryStore::default();
        let snap_a = Snapshot {
            machine_id: "counter".into(),
            state: "idle".into(),
            context: json!({ "count": 42 }),
            version: 1,
            last_event: Some("increment".into()),
        };
        store.store("session1", "counter", &snap_a).await.unwrap();
        let loaded = store.clone().load("session1", "counter").await.unwrap();
        assert_eq!(loaded.state, "idle");
        assert_eq!(loaded.context["count"], 42);
        assert_eq!(loaded.version, 1);
    }

    #[tokio::test]
    async fn ha_shared_store_history_visible_across_replicas() {
        let store = InMemoryStore::default();
        let store_b = store.clone();
        store.store("s", "m", &Snapshot { machine_id: "m".into(), state: "a".into(), context: json!({}), version: 1, last_event: None }).await.unwrap();
        store.store("s", "m", &Snapshot { machine_id: "m".into(), state: "b".into(), context: json!({}), version: 2, last_event: None }).await.unwrap();
        let history = store_b.history("s", "m").await;
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].state, "a");
        assert_eq!(history[1].state, "b");
    }
}
