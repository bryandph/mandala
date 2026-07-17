//! The leader side of the context endpoint: a bound loopback listener (the
//! bind IS the lock) serving protocol-v1 connections.
//!
//! Per connection: bearer hello before anything else, then a read loop that
//! spawns each `call` into its own task (calls run concurrently; a blocking
//! `deploy_status` never wedges the connection), forwards subscribed activity
//! events, answers pings, and — while any call is in flight — emits heartbeat
//! frames on a timer so the peer can tell "still working" from "dead".
//! All writes funnel through one mpsc-fed writer task, so concurrent call
//! settlements, events, and heartbeats never interleave mid-line.
//!
//! Execution is NOT this crate's business: every call is handed to the
//! injected [`Dispatch`] with the connection's client identity, and activity
//! reaches subscribers through the injected broadcast sender. The MCP
//! dispatch core (mandala-mcp) plugs in from above — this crate stays below
//! it in the dependency order, because followers of section 3 will need the
//! client half from inside mandala-mcp.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::protocol::{Frame, PROTOCOL_VERSION, UNAUTHORIZED};

/// A pending call execution.
pub type DispatchFuture = Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>;

/// The execution seam: `(origin client, tool, args) → result`. The origin is
/// the hello's `client` — the leader labels the call's activity/audit with it.
pub type Dispatch =
    Arc<dyn Fn(String, String, serde_json::Map<String, Value>) -> DispatchFuture + Send + Sync>;

/// How long a fresh connection has to complete its hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

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

/// Shared per-endpoint state.
struct HostShared {
    token: String,
    flake: String,
    config: HostConfig,
}

/// A live, serving context endpoint — holding this IS holding leadership;
/// dropping it (or process death) releases the bind and with it the lock.
pub struct RunningHost {
    port: u16,
    accept: JoinHandle<()>,
}

impl RunningHost {
    /// Serve an already-bound listener (the caller's successful bind is the
    /// acquired lock).
    ///
    /// # Panics
    /// The listener has no local address (cannot happen for a bound TCP
    /// listener).
    #[must_use]
    pub fn start(
        listener: TcpListener,
        token: String,
        flake: String,
        config: HostConfig,
    ) -> Self {
        let port = listener.local_addr().expect("bound listener has an addr").port();
        let shared = Arc::new(HostShared {
            token,
            flake,
            config,
        });
        let accept = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        tokio::spawn(serve_conn(stream, Arc::clone(&shared)));
                    }
                    Err(_) => {
                        // Transient accept failures (EMFILE, …): back off,
                        // keep the bind — releasing it would drop leadership.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        });
        Self { port, accept }
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

    /// Stop accepting and release the bind. Spike-grade shutdown: in-flight
    /// connections are not drained here — the orderly stop-accept → drain →
    /// close discipline is section-2 work (the Python quit-crash lesson).
    pub fn shutdown(self) {
        self.accept.abort();
    }
}

impl Drop for RunningHost {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

/// One connection's lifecycle: hello-auth, then serve frames until EOF.
async fn serve_conn(stream: TcpStream, shared: Arc<HostShared>) {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    // All frames leave through one writer task: concurrent call settlements,
    // events, and heartbeats stay line-atomic.
    let (tx, mut rx) = mpsc::channel::<Frame>(64);
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            let Ok(mut line) = frame.to_line() else {
                continue;
            };
            line.push('\n');
            if write_half.write_all(line.as_bytes()).await.is_err() {
                break; // peer gone; drop rx → senders see closed
            }
        }
    });

    // Bearer auth before anything else is served (fleet-context spec). The
    // rejection carries our flake so a colliding context's probe can move on.
    let hello = tokio::time::timeout(HELLO_TIMEOUT, lines.next_line()).await;
    let Ok(Ok(Some(first))) = hello else {
        drop(tx);
        let _ = writer.await;
        return;
    };
    let client = match Frame::from_line(&first) {
        Ok(Frame::Hello { token, client, .. }) if token == shared.token => client,
        _ => {
            let _ = tx
                .send(Frame::Error {
                    error: UNAUTHORIZED.to_string(),
                    flake: Some(shared.flake.clone()),
                })
                .await;
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
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(cadence);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                if tx.is_closed() {
                    break;
                }
                if in_flight.load(Ordering::Relaxed) > 0
                    && tx.send(Frame::Heartbeat).await.is_err()
                {
                    break;
                }
            }
        });
    }

    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        match Frame::from_line(&line) {
            Ok(Frame::Call { id, tool, args }) => {
                in_flight.fetch_add(1, Ordering::Relaxed);
                let tx = tx.clone();
                let shared = Arc::clone(&shared);
                let in_flight = Arc::clone(&in_flight);
                let origin = client.clone();
                tokio::spawn(async move {
                    let outcome = (shared.config.dispatch)(origin, tool, args).await;
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
                    let _ = tx.send(frame).await;
                });
            }
            Ok(Frame::Subscribe) => {
                let _ = tx.send(Frame::Subscribed).await;
                let mut events = shared.config.events.subscribe();
                let tx = tx.clone();
                tokio::spawn(async move {
                    loop {
                        match events.recv().await {
                            Ok(event) => {
                                if tx.send(Frame::Event { event }).await.is_err() {
                                    break;
                                }
                            }
                            // A lagged subscriber loses events, not the
                            // stream — same stance as the eval worker.
                            Err(broadcast::error::RecvError::Lagged(_)) => {}
                            Err(broadcast::error::RecvError::Closed) => break,
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
    // Reader EOF: drop our sender; the writer drains what call/subscribe
    // tasks still hold and exits when their sends start failing.
    drop(tx);
    let _ = writer.await;
}
