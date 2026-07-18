//! The leader side of the context endpoint: a bound loopback listener (the
//! bind IS the lock) serving protocol-v1 connections.
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
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

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
    /// Shutdown phase 1: accepted connections are dropped before the hello.
    refusing: AtomicBool,
    /// Shutdown phase 2 (or abrupt drop): every host task exits, closing its
    /// streams. `true` is terminal; a dropped sender reads the same way.
    shutdown: watch::Receiver<bool>,
}

impl HostShared {
    fn token_matches(&self, presented: &str) -> bool {
        *self.token.read().expect("token lock poisoned") == presented
    }
}

/// A live, serving context endpoint — holding this IS holding leadership;
/// dropping it (or process death) releases the bind and with it the lock.
pub struct RunningHost {
    port: u16,
    accept: JoinHandle<()>,
    shared: Arc<HostShared>,
    shutdown_tx: watch::Sender<bool>,
    claim: Option<Claim>,
}

impl RunningHost {
    /// Serve an already-bound listener (the caller's successful bind is the
    /// acquired lock).
    ///
    /// # Panics
    /// The listener has no local address (cannot happen for a bound TCP
    /// listener).
    #[must_use]
    pub fn start(listener: TcpListener, token: String, flake: String, config: HostConfig) -> Self {
        let port = listener
            .local_addr()
            .expect("bound listener has an addr")
            .port();
        let (shutdown_tx, shutdown) = watch::channel(false);
        let shared = Arc::new(HostShared {
            token: RwLock::new(token),
            flake,
            config,
            unsettled: AtomicUsize::new(0),
            refusing: AtomicBool::new(false),
            shutdown,
        });
        let accept = {
            let shared = Arc::clone(&shared);
            tokio::spawn(async move {
                let mut shutdown = shared.shutdown.clone();
                loop {
                    tokio::select! {
                        accepted = listener.accept() => match accepted {
                            Ok((stream, _)) => {
                                if shared.refusing.load(Ordering::Acquire) {
                                    // Shutdown phase 1: refuse before the
                                    // hello (the peer sees close-before-
                                    // welcome, a probe reads "not a context").
                                    drop(stream);
                                    continue;
                                }
                                tokio::spawn(serve_conn(stream, Arc::clone(&shared)));
                            }
                            Err(_) => {
                                // Transient accept failures (EMFILE, …): back
                                // off, keep the bind — releasing it would
                                // drop leadership.
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                        },
                        // Terminal: dropping out drops the listener — the
                        // port refuses connects, the lock is released.
                        () = shut_down(&mut shutdown) => break,
                    }
                }
            })
        };
        Self {
            port,
            accept,
            shared,
            shutdown_tx,
            claim: None,
        }
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
    /// clients see the connection close as a structured failover).
    pub async fn shutdown(self, grace: Duration) {
        self.shared.refusing.store(true, Ordering::Release);
        let deadline = tokio::time::Instant::now() + grace;
        while self.shared.unsettled.load(Ordering::Acquire) > 0
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(DRAIN_POLL).await;
        }
        let _ = self.shutdown_tx.send(true);
        // The accept task drops the listener when it exits; wait for that so
        // "shutdown returned" means "the port is closed".
        self.accept.abort();
        while !self.accept.is_finished() {
            tokio::time::sleep(Duration::from_millis(1)).await;
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
        // Abrupt death: kill everything NOW — no drain, and no discovery
        // release (the next claimant reuses the stale file's token, exactly
        // like a crashed process).
        let _ = self.shutdown_tx.send(true);
        self.accept.abort();
    }
}

/// One connection's lifecycle: hello-auth, then serve frames until EOF (or
/// host shutdown). When this task returns — peer EOF, protocol end, or
/// shutdown — the per-connection liveness watch drops and every subtask
/// (writer, heartbeat, subscription forwarders) exits promptly.
async fn serve_conn(stream: TcpStream, shared: Arc<HostShared>) {
    let (read_half, mut write_half) = stream.into_split();
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
                    broken = write_half.write_all(line.as_bytes()).await.is_err();
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
                let tx = tx.clone();
                let shared = Arc::clone(&shared);
                let in_flight = Arc::clone(&in_flight);
                let origin = client.clone();
                tokio::spawn(async move {
                    let outcome = (shared.config.dispatch)(Some(origin), tool, args).await;
                    in_flight.fetch_sub(1, Ordering::Relaxed);
                    let frame = match outcome {
                        Ok(result) => Frame::CallResult {
                            id,
                            ok: true,
                            result: Some(result),
                            error: None,
                        },
                        Err(error) => Frame::CallResult {
                            id,
                            ok: false,
                            result: None,
                            error: Some(error),
                        },
                    };
                    if tx.send(frame).await.is_err() {
                        // The writer is gone; the result can never be
                        // written — settle the drain counter ourselves.
                        shared.unsettled.fetch_sub(1, Ordering::AcqRel);
                    }
                });
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
