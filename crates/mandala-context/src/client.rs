//! The joining side: connect, hello, then call / subscribe / ping over one
//! multiplexed connection.
//!
//! A background reader task routes inbound frames: `result` frames settle
//! their pending oneshot by id, `event` frames feed the subscription channel,
//! `pong`/`subscribed` settle their waiters, and heartbeats bump a counter
//! (the caller's liveness signal during a long-blocking proxied call). Calls
//! carry client-minted ids, so several may be in flight on one connection.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};

use crate::protocol::{Frame, PROTOCOL_VERSION, UNAUTHORIZED};

/// How long a connect + hello handshake may take before the peer is judged
/// not-a-context (a foreign listener that never answers).
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Why joining an endpoint failed.
#[derive(Debug)]
pub enum ConnectError {
    /// TCP-level failure (refused/unreachable — a dead endpoint).
    Io(io::Error),
    /// The endpoint is a live context but our token is wrong. `server_flake`
    /// tells a port-collision probe WHOSE context answered.
    Unauthorized { server_flake: Option<String> },
    /// The listener did not speak protocol v1 (foreign service, garbage,
    /// silence, or close-before-welcome).
    NotAContext(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "endpoint unreachable: {e}"),
            Self::Unauthorized { server_flake } => match server_flake {
                Some(flake) => write!(f, "unauthorized (endpoint serves {flake})"),
                None => write!(f, "unauthorized"),
            },
            Self::NotAContext(why) => write!(f, "not a context endpoint: {why}"),
        }
    }
}

impl std::error::Error for ConnectError {}

/// Waiters the reader task settles.
#[derive(Default)]
struct Waiters {
    calls: HashMap<u64, oneshot::Sender<Result<Value, String>>>,
    ping: Option<oneshot::Sender<()>>,
    subscribed: Option<oneshot::Sender<()>>,
    events: Option<mpsc::Sender<Value>>,
}

/// A joined context connection (a follower's or observer's handle).
pub struct ContextClient {
    tx: mpsc::Sender<Frame>,
    waiters: Arc<Mutex<Waiters>>,
    heartbeats: Arc<AtomicU64>,
    next_id: AtomicU64,
    /// The leader's canonical flake path, from its welcome.
    pub server_flake: String,
    /// The leader's pid, from its welcome (advisory — never a liveness
    /// judgement).
    pub server_pid: u32,
}

impl ContextClient {
    /// Connect and complete the hello/welcome handshake.
    ///
    /// # Errors
    /// [`ConnectError`] — refused, unauthorized, or not a v1 context.
    pub async fn connect(
        addr: SocketAddr,
        token: &str,
        client_name: &str,
        flake: &str,
    ) -> Result<Self, ConnectError> {
        let handshake = async {
            let stream = TcpStream::connect(addr).await.map_err(ConnectError::Io)?;
            let (read_half, mut write_half) = stream.into_split();
            let mut lines = BufReader::new(read_half).lines();

            let hello = Frame::Hello {
                v: PROTOCOL_VERSION,
                token: token.to_string(),
                client: client_name.to_string(),
                flake: flake.to_string(),
            };
            let mut line = hello
                .to_line()
                .map_err(|e| ConnectError::NotAContext(e.to_string()))?;
            line.push('\n');
            write_half
                .write_all(line.as_bytes())
                .await
                .map_err(ConnectError::Io)?;

            let first = lines
                .next_line()
                .await
                .map_err(ConnectError::Io)?
                .ok_or_else(|| ConnectError::NotAContext("closed before welcome".to_string()))?;
            match Frame::from_line(&first) {
                Ok(Frame::Welcome { flake, pid, .. }) => Ok((lines, write_half, flake, pid)),
                Ok(Frame::Error { error, flake }) if error == UNAUTHORIZED => {
                    Err(ConnectError::Unauthorized {
                        server_flake: flake,
                    })
                }
                Ok(other) => Err(ConnectError::NotAContext(format!(
                    "unexpected first frame: {other:?}"
                ))),
                Err(_) => Err(ConnectError::NotAContext(
                    "non-protocol response".to_string(),
                )),
            }
        };
        let (mut lines, mut write_half, server_flake, server_pid) =
            tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake)
                .await
                .map_err(|_| ConnectError::NotAContext("handshake timeout".to_string()))??;

        let waiters = Arc::new(Mutex::new(Waiters::default()));
        let heartbeats = Arc::new(AtomicU64::new(0));

        // Writer: every outbound frame through one task, line-atomic.
        let (tx, mut rx) = mpsc::channel::<Frame>(64);
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                let Ok(mut line) = frame.to_line() else {
                    continue;
                };
                line.push('\n');
                if write_half.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });

        // Reader: route frames to their waiters until EOF, then fail every
        // pending call — a dropped leader must surface as a structured error,
        // never a hang.
        {
            let waiters = Arc::clone(&waiters);
            let heartbeats = Arc::clone(&heartbeats);
            tokio::spawn(async move {
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let Ok(frame) = Frame::from_line(&line) else {
                        continue;
                    };
                    let mut w = waiters.lock().expect("waiters lock poisoned");
                    match frame {
                        Frame::CallResult {
                            id, ok, result, error, ..
                        } => {
                            if let Some(sender) = w.calls.remove(&id) {
                                let outcome = if ok {
                                    Ok(result.unwrap_or(Value::Null))
                                } else {
                                    Err(error.unwrap_or_else(|| "call failed".to_string()))
                                };
                                let _ = sender.send(outcome);
                            }
                        }
                        Frame::Event { event } => {
                            if let Some(sink) = &w.events {
                                let _ = sink.try_send(event);
                            }
                        }
                        Frame::Pong => {
                            if let Some(sender) = w.ping.take() {
                                let _ = sender.send(());
                            }
                        }
                        Frame::Subscribed => {
                            if let Some(sender) = w.subscribed.take() {
                                let _ = sender.send(());
                            }
                        }
                        Frame::Heartbeat => {
                            heartbeats.fetch_add(1, Ordering::Relaxed);
                        }
                        _ => {}
                    }
                }
                let mut w = waiters.lock().expect("waiters lock poisoned");
                for (_, sender) in w.calls.drain() {
                    let _ = sender.send(Err("context connection closed".to_string()));
                }
            });
        }

        Ok(Self {
            tx,
            waiters,
            heartbeats,
            next_id: AtomicU64::new(1),
            server_flake,
            server_pid,
        })
    }

    /// Execute one tool call at the leader. `Err` carries either the tool's
    /// own error text or the structured connection-loss message.
    ///
    /// # Errors
    /// The forwarded call failed, or the connection dropped mid-call.
    ///
    /// # Panics
    /// The waiter lock is poisoned (a routing task panicked).
    pub async fn call(
        &self,
        tool: &str,
        args: serde_json::Map<String, Value>,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (otx, orx) = oneshot::channel();
        self.waiters
            .lock()
            .expect("waiters lock poisoned")
            .calls
            .insert(id, otx);
        self.tx
            .send(Frame::Call {
                id,
                tool: tool.to_string(),
                args,
            })
            .await
            .map_err(|_| "context connection closed".to_string())?;
        orx.await
            .map_err(|_| "context connection closed".to_string())?
    }

    /// Subscribe to the leader's activity stream. Resolves once the server
    /// acks (`subscribed`), so events published after this returns are
    /// guaranteed to flow.
    ///
    /// # Errors
    /// The connection dropped, or the ack never arrived.
    ///
    /// # Panics
    /// The waiter lock is poisoned (a routing task panicked).
    pub async fn subscribe(&self) -> Result<mpsc::Receiver<Value>, String> {
        let (etx, erx) = mpsc::channel(256);
        let (atx, arx) = oneshot::channel();
        {
            let mut w = self.waiters.lock().expect("waiters lock poisoned");
            w.events = Some(etx);
            w.subscribed = Some(atx);
        }
        self.tx
            .send(Frame::Subscribe)
            .await
            .map_err(|_| "context connection closed".to_string())?;
        tokio::time::timeout(HANDSHAKE_TIMEOUT, arx)
            .await
            .map_err(|_| "subscribe ack timeout".to_string())?
            .map_err(|_| "context connection closed".to_string())?;
        Ok(erx)
    }

    /// Liveness roundtrip: `true` iff the leader ponged in time.
    ///
    /// # Panics
    /// The waiter lock is poisoned (a routing task panicked).
    pub async fn ping(&self) -> bool {
        let (ptx, prx) = oneshot::channel();
        self.waiters
            .lock()
            .expect("waiters lock poisoned")
            .ping = Some(ptx);
        if self.tx.send(Frame::Ping).await.is_err() {
            return false;
        }
        tokio::time::timeout(HANDSHAKE_TIMEOUT, prx).await.is_ok()
    }

    /// Heartbeat frames seen so far (rises while a long call is in flight).
    #[must_use]
    pub fn heartbeats_seen(&self) -> u64 {
        self.heartbeats.load(Ordering::Relaxed)
    }
}
