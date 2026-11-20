//! The leader side: a loopback listener serving protocol-v1 connections,
//! owned together with the lifetime coordination lease on acquisition.
//!
//! Per connection: bearer hello before anything else, then a read loop that
//! spawns each `call` into its own task (calls run concurrently; a blocking
//! `deploy_status` never wedges the connection), forwards subscribed activity
//! events, answers pings, and — while any call is in flight — emits heartbeat
//! frames on a timer so the peer can tell "still working" from "dead".
//! All writes funnel through one mpsc-fed writer task, so concurrent call
//! settlements, events, and heartbeats never interleave mid-line. A dead
//! peer is reaped at read-EOF: every per-connection task (subscription
//! forwarders, the heartbeat timer, the writer) watches the connection's
//! liveness and exits promptly, releasing its broadcast receiver — never
//! waiting for the next event send to fail.
//!
//! Orderly shutdown ([`RunningHost::shutdown`]) is the Python quit-crash
//! lesson made explicit: stop serving NEW connections (accepted, then
//! dropped before the hello — the probe's "not a context" signal) → let
//! in-flight calls drain within a bounded grace (a call counts as settled
//! only once its result frame is written to the socket) → close subscriber
//! streams and the listener (the port refuses connects; followers re-race) →
//! release the discovery claim (guarded: only if the file still records this
//! leader). An abrupt `Drop` skips the drain and the discovery release —
//! that is deliberate leader-death semantics (stale discovery, token reuse).
//!
//! Execution is NOT this crate's business: every call is handed to the
//! injected [`Dispatch`] with the connection's client identity, and activity
//! reaches subscribers through the injected broadcast sender. The MCP
//! dispatch core (mandala-mcp) plugs in from above — this crate stays below
//! it in the dependency order, because followers consume the client half
//! from inside mandala-mcp.

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle, JoinSet};

use crate::discovery::{self, Discovery};
use crate::protocol::{Frame, PROTOCOL_VERSION, UNAUTHORIZED};

/// A pending call execution.
pub type DispatchFuture = Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>;

/// The execution seam: `(origin, tool, args) → result`. The origin is the
/// hello's `client` identity for calls that arrived over the wire — the
/// leader labels the call's activity/audit with it — and `None` for the
/// leader's own local calls (which carry no origin).
pub type Dispatch = Arc<
    dyn Fn(Option<String>, String, serde_json::Map<String, Value>) -> DispatchFuture + Send + Sync,
>;

/// How long a fresh connection has to complete its hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// Resolve once the host is shutting down — the value turned `true`, or the
/// sender (the [`RunningHost`]) is gone entirely. A `changed()` loop rather
/// than `wait_for` because the latter's output holds a non-`Send` value
/// guard across select arms.
async fn shut_down(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// How often the shutdown drain re-checks the unsettled-call counter.
const DRAIN_POLL: Duration = Duration::from_millis(10);

/// What the endpoint serves with (dispatch, activity, heartbeat cadence).
#[derive(Clone)]
pub struct HostConfig {
    /// Call execution (the leader's MCP dispatch core).
    pub dispatch: Dispatch,
    /// The activity stream fanned out to subscribers. The leader publishes
    /// its dispatch-wrapper events here (origin-labeled).
    pub events: broadcast::Sender<Value>,
    /// Heartbeat cadence for connections with calls in flight. Production
    /// default [`HostConfig::DEFAULT_HEARTBEAT`]; tests shrink it.
    pub heartbeat_interval: Duration,
    /// Leader-owned background work (not independent deploy supervisors).
    /// Teardown aborts and joins these before releasing leadership.
    pub background_tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl HostConfig {
    /// Comfortable under any sane idle-connection killer, frequent enough
    /// that a 570s `deploy_status` wait is visibly alive.
    pub const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(15);

    /// Config with the production heartbeat cadence.
    #[must_use]
    pub fn new(dispatch: Dispatch, events: broadcast::Sender<Value>) -> Self {
        Self {
            dispatch,
            events,
            heartbeat_interval: Self::DEFAULT_HEARTBEAT,
            background_tasks: Arc::default(),
        }
    }
}

/// Where the leader's discovery file lives (set by the acquire path; direct
/// `RunningHost::start` callers — protocol tests — have none).
struct Claim {
    state_dir: PathBuf,
    key: String,
}

/// Shared per-endpoint state.
struct HostShared {
    /// The accepted bearer token — swappable by [`RunningHost::rotate_token`]
    /// (auth happens only at hello, so rotation cuts NEW connections while
    /// authenticated ones live out their lives).
    token: RwLock<String>,
    flake: String,
    config: HostConfig,
    /// Calls accepted but whose result frame is not yet written to (or
    /// abandoned by) a socket — the shutdown drain's truth. Decremented by
    /// the writer AFTER the write, so "zero" means every settled result
    /// actually reached the wire.
    unsettled: AtomicUsize,
    active: AtomicUsize,
    writers: AtomicUsize,
    /// Shutdown phase 1: accepted connections are dropped before the hello.
    refusing: AtomicBool,
    /// Shutdown phase 2 (or abrupt drop): every host task exits, closing its
    /// streams. `true` is terminal; a dropped sender reads the same way.
    shutdown: watch::Receiver<bool>,
    calls: mpsc::UnboundedSender<LocalCall>,
    // Every task that can invoke config retains this shared owner; its final
    // drop cannot precede task termination or the owner's listener closure.
    work: crate::work::WorkOwner,
}

struct AbortOnDrop(AbortHandle);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct ActiveCall(Arc<HostShared>);
impl Drop for ActiveCall {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

struct LocalCall {
    active: ActiveCall,
    origin: Option<String>,
    tool: String,
    args: serde_json::Map<String, Value>,
    reply: Reply,
}

enum Reply {
    Local(oneshot::Sender<Result<Value, String>>),
    Wire(WireReply),
}
struct WireReply {
    id: u64,
    tx: mpsc::Sender<Frame>,
    in_flight: Arc<AtomicUsize>,
    shared: Arc<HostShared>,
    counting_flight: bool,
    counting_settlement: bool,
}
impl WireReply {
    async fn send(mut self, outcome: Result<Value, String>) {
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        self.counting_flight = false;
        if *self.shared.shutdown.borrow() || self.shared.work.scope().closed() {
            return;
        }
        let frame = match outcome {
            Ok(result) => Frame::CallResult {
                id: self.id,
                ok: true,
                result: Some(result),
                error: None,
            },
            Err(error) => Frame::CallResult {
                id: self.id,
                ok: false,
                result: None,
                error: Some(error),
            },
        };
        if self.tx.send(frame).await.is_ok() {
            self.counting_settlement = false;
        }
    }
}
impl Drop for WireReply {
    fn drop(&mut self) {
        if self.counting_flight {
            self.in_flight.fetch_sub(1, Ordering::Relaxed);
        }
        if self.counting_settlement {
            self.shared.unsettled.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
struct LiveWriter(Arc<HostShared>);
impl Drop for LiveWriter {
    fn drop(&mut self) {
        self.0.writers.fetch_sub(1, Ordering::AcqRel);
    }
}
// Actual stream destruction precedes the counter used by ordered teardown.
struct WriterSocket {
    stream: tokio::net::tcp::OwnedWriteHalf,
    _live: LiveWriter,
}
impl WriterSocket {
    async fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stream.write_all(bytes).await
    }
}

/// Single teardown owner. Field order protects runtime cancellation too:
/// listener closes before tracked tasks and the final shared lease owner.
struct HostOwner {
    listener: Option<TcpListener>,
    connections: JoinSet<()>,
    async_calls: JoinSet<()>,
    shared: Arc<HostShared>,
    calls: mpsc::UnboundedReceiver<LocalCall>,
    resources_closed: watch::Sender<bool>,
}

impl Drop for HostOwner {
    fn drop(&mut self) {
        self.shared.work.scope().close();
        // Normal shutdown awaits these; runtime cancellation must at least
        // cancel them, never detach leader-owned background work.
        let background = std::mem::take(
            &mut *self
                .shared
                .config
                .background_tasks
                .lock()
                .expect("background tasks poisoned"),
        );
        if !background.is_empty() {
            for task in &background {
                task.abort();
            }
            // This path is runtime cancellation, not ordinary host Drop (the
            // owner is never explicitly aborted). A runtime-independent reaper
            // retains the lease until background futures are actually gone.
            // Fail closed by leaking the retained owner if thread creation fails.
            let retained = Arc::clone(&self.shared);
            let retained = Arc::into_raw(retained) as usize;
            let result = std::thread::Builder::new()
                .name("context-teardown".into())
                .spawn(move || {
                    // SAFETY: exactly one Arc raw reference was transferred here;
                    // on spawn failure it is intentionally retained, never released.
                    let _retained = unsafe { Arc::from_raw(retained as *const HostShared) };
                    while background.iter().any(|task| !task.is_finished()) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                });
            if let Ok(thread) = result {
                drop(thread);
            }
        }
    }
}

impl HostOwner {
    async fn run(mut self) {
        let mut shutdown = self.shared.shutdown.clone();
        loop {
            tokio::select! {
                biased;
                () = shut_down(&mut shutdown) => break,
                Some(call) = self.calls.recv() => {
                    let shared = Arc::clone(&self.shared);
                    self.async_calls.spawn(async move {
                        let LocalCall { active, origin, tool, args, reply } = call;
                        let _active = active;
                        let scope = shared.work.scope();
                        let result = crate::work::in_scope(Some(scope), async move {
                            (shared.config.dispatch)(origin, tool, args).await
                        }).await.unwrap_or_else(|error| Err(error.to_string()));
                        match reply {
                            Reply::Local(reply) => { let _ = reply.send(result); },
                            Reply::Wire(reply) => reply.send(result).await,
                        }
                    });
                }
                accepted = self.listener.as_ref().expect("live listener").accept() => match accepted {
                    Ok((stream, _)) => {
                        if self.shared.refusing.load(Ordering::Acquire) { drop(stream); continue; }
                        self.connections.spawn(serve_conn(stream, Arc::clone(&self.shared)));
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                },
                Some(_) = self.connections.join_next(), if !self.connections.is_empty() => {},
                Some(_) = self.async_calls.join_next(), if !self.async_calls.is_empty() => {},
            }
        }
        self.shared.work.scope().close();
        // Ordered retirement: listener/connections, calls, watcher, blocking
        // jobs, and only then the final guard. Queued jobs cannot start.
        drop(self.listener.take());
        self.connections.abort_all();
        while self.connections.join_next().await.is_some() {}
        while self.shared.writers.load(Ordering::Acquire) > 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        self.calls.close();
        while let Ok(call) = self.calls.try_recv() {
            drop(call);
        }
        self.async_calls.abort_all();
        while self.async_calls.join_next().await.is_some() {}
        let background = std::mem::take(
            &mut *self
                .shared
                .config
                .background_tasks
                .lock()
                .expect("background tasks poisoned"),
        );
        for task in &background {
            task.abort();
        }
        for task in background {
            let _ = task.await;
        }
        let _ = self.resources_closed.send(true);
        // Public shutdown may return after resource closure, preserving bounded
        // frontend quit. This owner and each running job retain the lease until
        // actual job completion; a hung job fails closed rather than unlocking.
        self.shared.work.scope().wait().await;
    }
}

impl HostShared {
    fn queue(
        self: &Arc<Self>,
        origin: Option<String>,
        tool: String,
        args: serde_json::Map<String, Value>,
    ) -> Result<oneshot::Receiver<Result<Value, String>>, String> {
        if *self.shutdown.borrow() || self.work.scope().closed() {
            return Err("context closed".into());
        }
        let (result, rx) = oneshot::channel();
        self.active.fetch_add(1, Ordering::AcqRel);
        let active = ActiveCall(Arc::clone(self));
        self.calls
            .send(LocalCall {
                active,
                origin,
                tool,
                args,
                reply: Reply::Local(result),
            })
            .map_err(|_| "context closed".to_string())?;
        Ok(rx)
    }

    fn queue_wire(
        self: &Arc<Self>,
        origin: String,
        tool: String,
        args: serde_json::Map<String, Value>,
        reply: WireReply,
    ) {
        if *self.shutdown.borrow() || self.work.scope().closed() {
            return;
        }
        self.active.fetch_add(1, Ordering::AcqRel);
        let active = ActiveCall(Arc::clone(self));
        let _ = self.calls.send(LocalCall {
            active,
            origin: Some(origin),
            tool,
            args,
            reply: Reply::Wire(reply),
        });
    }

    fn token_matches(&self, presented: &str) -> bool {
        *self.token.read().expect("token lock poisoned") == presented
    }
}

/// A live endpoint. Acquired hosts retain leadership until their listener
/// and dispatched work have actually stopped; Drop requests owned teardown.
pub struct RunningHost {
    port: u16,
    accept: JoinHandle<()>,
    shared: Arc<HostShared>,
    shutdown_tx: watch::Sender<bool>,
    claim: Option<Claim>,
    resources_closed: watch::Receiver<bool>,
}

impl RunningHost {
    /// Serve an already-bound listener without an election lease (the
    /// isolated protocol-test seam). Production acquisition uses start_guarded.
    ///
    /// # Panics
    /// The listener has no local address (cannot happen for a bound TCP
    /// listener).
    #[must_use]
    pub fn start(listener: TcpListener, token: String, flake: String, config: HostConfig) -> Self {
        Self::start_guarded(
            listener,
            token,
            flake,
            config,
            crate::work::WorkOwner::new(None),
        )
    }

    pub(crate) fn start_guarded(
        listener: TcpListener,
        token: String,
        flake: String,
        config: HostConfig,
        work: crate::work::WorkOwner,
    ) -> Self {
        let port = listener
            .local_addr()
            .expect("bound listener has an addr")
            .port();
        let (shutdown_tx, shutdown) = watch::channel(false);
        let (calls, call_rx) = mpsc::unbounded_channel();
        let (resources_tx, resources_closed) = watch::channel(false);
        let shared = Arc::new(HostShared {
            token: RwLock::new(token),
            flake,
            config,
            unsettled: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            writers: AtomicUsize::new(0),
            refusing: AtomicBool::new(false),
            shutdown,
            calls,
            work,
        });
        let owner = HostOwner {
            listener: Some(listener),
            connections: JoinSet::new(),
            async_calls: JoinSet::new(),
            shared: Arc::clone(&shared),
            calls: call_rx,
            resources_closed: resources_tx,
        };
        let accept = tokio::spawn(owner.run());
        Self {
            port,
            accept,
            shared,
            shutdown_tx,
            claim: None,
            resources_closed,
        }
    }

    /// A non-owning local snapshot. Execution is registered with the lease's
    /// task owner, never by retaining callable unguarded configuration.
    pub(crate) fn local_dispatch(&self) -> Dispatch {
        let shared = Arc::downgrade(&self.shared);
        Arc::new(move |origin, tool, args| {
            let shared = shared.clone();
            Box::pin(async move {
                let shared = shared.upgrade().ok_or("context closed")?;
                if *shared.shutdown.borrow() || shared.refusing.load(Ordering::Acquire) {
                    return Err("context closed".into());
                }
                let rx = shared.queue(origin, tool, args)?;
                drop(shared);
                rx.await.map_err(|_| "context closed".to_string())?
            })
        })
    }

    /// Record where this leader's discovery file lives, enabling
    /// [`RunningHost::rotate_token`]'s rewrite and [`RunningHost::shutdown`]'s
    /// guarded release. The acquire path sets this; hosts without a claim
    /// simply skip both.
    #[must_use]
    pub fn published_at(mut self, state_dir: &Path, key: &str) -> Self {
        self.claim = Some(Claim {
            state_dir: state_dir.to_path_buf(),
            key: key.to_string(),
        });
        self
    }

    /// The actually-bound port (recorded in discovery; authoritative).
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The discovery `url` for this endpoint.
    #[must_use]
    pub fn url(&self) -> String {
        format!("tcp://127.0.0.1:{}", self.port)
    }

    /// Rotate the context's bearer token: mint a fresh one, swap it in as the
    /// only accepted token, and rewrite the discovery file around it.
    ///
    /// Cutoff semantics (decision of record): auth happens only at hello, so
    /// clients holding the old token are cut off at their NEXT connect —
    /// already-authenticated connections (and their in-flight calls) live out
    /// their lives untouched. Joiners re-read discovery and converge on the
    /// fresh token; a joiner racing the rewrite lands on the stale-token
    /// path (`unauthorized` carrying this flake) and re-reads discovery —
    /// the same convergence as the mint race.
    ///
    /// # Errors
    /// The discovery rewrite failed (the swapped token is already in force;
    /// the caller should retry or surface it — a discovery file with the OLD
    /// token self-heals through the stale-token poll only until it expires,
    /// so an unwritable state dir here is worth surfacing loudly).
    ///
    /// # Panics
    /// The token lock is poisoned.
    pub fn rotate_token(&self) -> io::Result<String> {
        let fresh = discovery::mint_token();
        *self.shared.token.write().expect("token lock poisoned") = fresh.clone();
        if let Some(claim) = &self.claim {
            discovery::write(
                &claim.state_dir,
                &claim.key,
                &Discovery {
                    url: self.url(),
                    token: fresh.clone(),
                    pid: std::process::id(),
                    flake: self.shared.flake.clone(),
                },
            )?;
        }
        Ok(fresh)
    }

    /// Orderly shutdown — stop accepting, drain, close, release:
    ///
    /// 1. new connections are accepted-then-dropped (refused before hello);
    /// 2. in-flight calls drain within `grace` — a call is drained only once
    ///    its result frame is WRITTEN to its client's socket;
    /// 3. every connection task exits: subscriber streams and call
    ///    connections close, the listener drops, the port refuses connects
    ///    (followers detect and re-race the bind);
    /// 4. the discovery claim is released — guarded, removed only if the
    ///    file still records this leader (a racer that already claimed is
    ///    never clobbered).
    ///
    /// Calls still unsettled when `grace` expires are abandoned (their
    /// clients see the connection close as a structured failover). The
    /// listener, connections, async calls and watcher are actually stopped
    /// before return. A retired owner retains the guards until registered
    /// blocking work completes; takeover may remain safely busy meanwhile.
    pub async fn shutdown(self, grace: Duration) {
        self.shutdown_inner(grace, false).await;
    }

    pub(crate) async fn shutdown_complete(self, grace: Duration) {
        self.shutdown_inner(grace, true).await;
    }

    async fn shutdown_inner(&self, grace: Duration, mut complete: bool) {
        self.shared.refusing.store(true, Ordering::Release);
        let deadline = tokio::time::Instant::now() + grace;
        while (self.shared.unsettled.load(Ordering::Acquire) > 0
            || self.shared.active.load(Ordering::Acquire) > 0)
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(DRAIN_POLL).await;
        }
        self.shared.work.scope().close();
        let _ = self.shutdown_tx.send(true);
        let mut resources = self.resources_closed.clone();
        while !*resources.borrow() {
            if resources.changed().await.is_err() {
                complete = true;
                break;
            }
        }
        if complete {
            while !self.accept.is_finished() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            self.shared.work.scope().wait().await;
            while Arc::strong_count(&self.shared) > 1 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
        if let Some(claim) = &self.claim {
            let ours = discovery::read(&claim.state_dir, &claim.key)
                .is_some_and(|d| d.pid == std::process::id() && d.url == self.url());
            if ours {
                let _ =
                    std::fs::remove_file(discovery::discovery_path(&claim.state_dir, &claim.key));
            }
        }
    }
}

impl Drop for RunningHost {
    fn drop(&mut self) {
        self.shared.work.scope().close();
        // Abrupt death: kill everything NOW — no drain, and no discovery
        // release (the next claimant reuses the stale file's token, exactly
        // like a crashed process).
        let _ = self.shutdown_tx.send(true);
        // Owner observes shutdown, aborts AND joins all dispatched work, then
        // drops the listener and lease. Never detach teardown by aborting it.
    }
}

/// One connection's lifecycle: hello-auth, then serve frames until EOF (or
/// host shutdown). When this task returns — peer EOF, protocol end, or
/// shutdown — the per-connection liveness watch drops and every subtask
/// (writer, heartbeat, subscription forwarders) exits promptly.
async fn serve_conn(stream: TcpStream, shared: Arc<HostShared>) {
    let (read_half, write_half) = stream.into_split();
    shared.writers.fetch_add(1, Ordering::AcqRel);
    let mut socket = WriterSocket {
        stream: write_half,
        _live: LiveWriter(Arc::clone(&shared)),
    };
    let mut lines = BufReader::new(read_half).lines();
    let mut shutdown = shared.shutdown.clone();

    // Dropped when this task returns: subtasks watch it and exit promptly —
    // a dead subscriber is reaped at read-EOF, not at its next event send.
    let (conn_alive_tx, conn_alive) = watch::channel(());

    // All frames leave through one writer task: concurrent call settlements,
    // events, and heartbeats stay line-atomic. The writer owns the unsettled
    // accounting: a call result counts as drained only after its write.
    let (tx, mut rx) = mpsc::channel::<Frame>(64);
    let writer = {
        let shared = Arc::clone(&shared);
        let mut shutdown = shared.shutdown.clone();
        tokio::spawn(async move {
            let mut broken = false;
            loop {
                let frame = tokio::select! {
                    f = rx.recv() => match f {
                        Some(f) => f,
                        None => break, // every sender gone; queue empty
                    },
                    () = shut_down(&mut shutdown) => break,
                };
                let settles = matches!(frame, Frame::CallResult { .. });
                if !broken && let Ok(mut line) = frame.to_line() {
                    line.push('\n');
                    broken = socket.write_all(line.as_bytes()).await.is_err();
                }
                if settles {
                    shared.unsettled.fetch_sub(1, Ordering::AcqRel);
                }
            }
            // Whatever is still queued will never be written; settle the
            // drain counter for the results among it.
            rx.close();
            while let Ok(frame) = rx.try_recv() {
                if matches!(frame, Frame::CallResult { .. }) {
                    shared.unsettled.fetch_sub(1, Ordering::AcqRel);
                }
            }
        })
    };

    // If owner teardown cancels this connection while a peer has stopped
    // reading, cancel its writer too. The writer retains the shared lease
    // until its write future/stream have actually been destroyed.
    let _writer_abort = AbortOnDrop(writer.abort_handle());

    // Bearer auth before anything else is served (fleet-context spec). The
    // rejection carries our flake so a colliding context's probe can move on.
    let hello = tokio::time::timeout(HELLO_TIMEOUT, lines.next_line()).await;
    let Ok(Ok(Some(first))) = hello else {
        drop(conn_alive_tx);
        drop(tx);
        let _ = writer.await;
        return;
    };
    let client = match Frame::from_line(&first) {
        Ok(Frame::Hello { token, client, .. }) if shared.token_matches(&token) => client,
        _ => {
            let _ = tx
                .send(Frame::Error {
                    error: UNAUTHORIZED.to_string(),
                    flake: Some(shared.flake.clone()),
                })
                .await;
            drop(conn_alive_tx);
            drop(tx);
            let _ = writer.await;
            return;
        }
    };
    let _ = tx
        .send(Frame::Welcome {
            v: PROTOCOL_VERSION,
            flake: shared.flake.clone(),
            pid: std::process::id(),
        })
        .await;

    // Heartbeats: only while calls are in flight — an idle connection is
    // probed by ping, a working one proves itself unprompted.
    let in_flight = Arc::new(AtomicUsize::new(0));
    {
        let tx = tx.clone();
        let in_flight = Arc::clone(&in_flight);
        let cadence = shared.config.heartbeat_interval;
        let mut conn_alive = conn_alive.clone();
        let mut shutdown = shared.shutdown.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(cadence);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        if in_flight.load(Ordering::Relaxed) > 0
                            && tx.send(Frame::Heartbeat).await.is_err()
                        {
                            break;
                        }
                    }
                    // Fires only when the connection task drops its end.
                    _ = conn_alive.changed() => break,
                    () = shut_down(&mut shutdown) => break,
                }
            }
        });
    }

    loop {
        let line = tokio::select! {
            l = lines.next_line() => match l {
                Ok(Some(line)) => line,
                _ => break, // peer EOF or read error: the connection is done
            },
            () = shut_down(&mut shutdown) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        match Frame::from_line(&line) {
            Ok(Frame::Call { id, tool, args }) => {
                shared.unsettled.fetch_add(1, Ordering::AcqRel);
                in_flight.fetch_add(1, Ordering::Relaxed);
                let reply = WireReply {
                    id,
                    tx: tx.clone(),
                    in_flight: Arc::clone(&in_flight),
                    shared: Arc::clone(&shared),
                    counting_flight: true,
                    counting_settlement: true,
                };
                shared.queue_wire(client.clone(), tool, args, reply);
            }
            Ok(Frame::Subscribe) => {
                let _ = tx.send(Frame::Subscribed).await;
                let mut events = shared.config.events.subscribe();
                let tx = tx.clone();
                let mut conn_alive = conn_alive.clone();
                let mut shutdown = shared.shutdown.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            received = events.recv() => match received {
                                Ok(event) => {
                                    if tx.send(Frame::Event { event }).await.is_err() {
                                        break;
                                    }
                                }
                                // A lagged subscriber loses events, not the
                                // stream — and, because the broadcast sender
                                // never blocks, a slow subscriber can never
                                // stall the call path.
                                Err(broadcast::error::RecvError::Lagged(_)) => {}
                                Err(broadcast::error::RecvError::Closed) => break,
                            },
                            // The connection died (read-EOF) — release the
                            // broadcast receiver NOW.
                            _ = conn_alive.changed() => break,
                            () = shut_down(&mut shutdown) => break,
                        }
                    }
                });
            }
            Ok(Frame::Ping) => {
                let _ = tx.send(Frame::Pong).await;
            }
            Ok(_) => {
                let _ = tx
                    .send(Frame::Error {
                        error: "unexpected frame".to_string(),
                        flake: None,
                    })
                    .await;
            }
            Err(e) => {
                let _ = tx
                    .send(Frame::Error {
                        error: format!("bad frame: {e}"),
                        flake: None,
                    })
                    .await;
            }
        }
    }
    // Reader done: drop the liveness watch (reaps heartbeat + subscription
    // tasks promptly) and our writer handle; the writer drains what call
    // tasks still hold and exits when their sends start failing.
    drop(conn_alive_tx);
    drop(tx);
    let _ = writer.await;
}
