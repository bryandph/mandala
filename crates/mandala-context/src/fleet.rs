//! The `FleetContext` seam — one trait over "execute here" and "forward to
//! the leader" — plus the follower promotion state machine.
//!
//! Tool code (the MCP server's dispatch path; later the TUI and CLI reads)
//! consumes [`FleetContext`] and never learns which side of the wire it is
//! on. Two implementations:
//!
//! - [`LocalContext`]: leader-local — executes via the injected [`Dispatch`]
//!   directly (origin `None`: the leader's own calls carry no origin label).
//! - [`ContextSession`]: the joined-context state machine — forwards via
//!   [`ContextClient`] while a leader answers, and on connection loss
//!   re-races the bind: the winner PROMOTES (its [`HostConfigFactory`] hook
//!   supplies a fresh dispatch — eval-worker spin-up is the caller's
//!   business), losers reconnect to the new leader.
//!
//! Retry discipline (fleet-context spec, enforced here, declared per call by
//! the caller): an in-flight forwarded call that dies with the leader
//! surfaces a structured [`CallError::Failover`]; calls marked idempotent
//! are retried ONCE after promotion/reconnect; mutations never — whether the
//! dead leader executed them is unknowable, and the run registry (not a
//! retry) is what tells the client what actually launched.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{Mutex, broadcast, mpsc};

use crate::client::{CallFailure, ContextClient};
use crate::host::{Dispatch, HostConfig, RunningHost};
use crate::identity::ContextIdentity;
use crate::{AcquireError, Acquired, acquire};

/// Produces the leader-side [`HostConfig`] — the promotion hook. Invoked
/// exactly when this process becomes (or re-becomes) the leader, so the
/// dispatch behind it is always fresh for the promotion it serves.
pub type HostConfigFactory = Arc<dyn Fn() -> HostConfig + Send + Sync>;

/// Why a context call failed, at the seam's level.
#[derive(Debug)]
pub enum CallError {
    /// The executing side answered: the tool itself failed with this
    /// message. The call ran — this is never a failover.
    Tool(String),
    /// The leader connection died mid-call — the structured failover error.
    /// Whether the dead leader executed the call is unknown.
    Failover {
        /// Whether a post-promotion retry was attempted (idempotent calls
        /// only; a `true` here means the retry ALSO lost its leader).
        retried: bool,
        /// Whether this process promoted to leader during the re-race
        /// (`false`: it reconnected to another winner).
        promoted: bool,
    },
    /// Re-acquiring the context after a lost leader failed outright.
    Acquire(AcquireError),
    /// The session was shut down.
    Closed,
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tool(msg) => write!(f, "{msg}"),
            Self::Failover { retried, promoted } => write!(
                f,
                "leader connection lost mid-call (execution state unknown; retried={retried}, promoted={promoted})"
            ),
            Self::Acquire(e) => write!(f, "context re-acquisition failed: {e}"),
            Self::Closed => write!(f, "context session closed"),
        }
    }
}

impl std::error::Error for CallError {}

/// The execution seam every frontend consumes — the swap point between
/// leader-local execution and follower forwarding, invisible to tool code.
#[async_trait]
pub trait FleetContext: Send + Sync {
    /// Execute one tool call. `idempotent` is the caller's declaration that
    /// the call is retry-safe across leader failover (reads); forwarding
    /// implementations enforce it — mutations are NEVER retried.
    async fn call(
        &self,
        tool: &str,
        args: serde_json::Map<String, Value>,
        idempotent: bool,
    ) -> Result<Value, CallError>;

    /// Subscribe to the context's activity stream.
    async fn subscribe(&self) -> Result<mpsc::Receiver<Value>, CallError>;
}

/// Leader-local execution: calls go straight into the injected dispatch
/// (origin `None` — a leader's own calls carry no origin label), and
/// subscriptions read the local activity broadcast.
pub struct LocalContext {
    dispatch: Dispatch,
    events: broadcast::Sender<Value>,
}

impl LocalContext {
    /// A local seam over the leader's dispatch and activity stream (the same
    /// pair its [`HostConfig`] serves remote callers with).
    #[must_use]
    pub fn new(dispatch: Dispatch, events: broadcast::Sender<Value>) -> Self {
        Self { dispatch, events }
    }
}

#[async_trait]
impl FleetContext for LocalContext {
    async fn call(
        &self,
        tool: &str,
        args: serde_json::Map<String, Value>,
        _idempotent: bool,
    ) -> Result<Value, CallError> {
        (self.dispatch)(None, tool.to_string(), args)
            .await
            .map_err(CallError::Tool)
    }

    async fn subscribe(&self) -> Result<mpsc::Receiver<Value>, CallError> {
        Ok(forward_broadcast(self.events.subscribe()))
    }
}

/// Pump a broadcast receiver into a bounded mpsc until either side closes.
/// Lag drops events, never the stream (the eval-worker stance).
fn forward_broadcast(mut events: broadcast::Receiver<Value>) -> mpsc::Receiver<Value> {
    let (tx, rx) = mpsc::channel(256);
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if tx.send(event).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    rx
}

/// What the session currently is.
enum Role {
    /// This process IS the leader: it holds the bind and executes locally.
    Leader {
        host: RunningHost,
        /// The promoted config, kept so local calls and subscriptions use
        /// exactly what remote callers are served with.
        config: HostConfig,
    },
    /// A live leader answers: calls forward over this connection.
    Follower { client: Arc<ContextClient> },
    /// Explicitly shut down.
    Closed,
}

/// One executable snapshot of the role (taken under the lock, executed
/// outside it so a slow call never blocks re-acquisition).
enum Exec {
    Local(Dispatch),
    Remote(Arc<ContextClient>),
}

struct SessionInner {
    identity: ContextIdentity,
    state_dir: PathBuf,
    client_name: String,
    make_host: HostConfigFactory,
    role: Mutex<Role>,
}

/// The joined-context state machine: leader or follower now, re-racing the
/// bind whenever the leader dies. Cheap to clone (all clones share one
/// role).
#[derive(Clone)]
pub struct ContextSession {
    inner: Arc<SessionInner>,
}

/// Backoff between subscription re-attach attempts after a failure.
const RESUBSCRIBE_BACKOFF: Duration = Duration::from_millis(50);

impl ContextSession {
    /// Join (or claim) the context: exactly [`acquire`], with the leader-side
    /// config coming from `make_host` — which is also the PROMOTION hook,
    /// invoked again on every later promotion.
    ///
    /// # Errors
    /// [`AcquireError`] from the underlying acquisition.
    pub async fn acquire(
        identity: ContextIdentity,
        state_dir: impl Into<PathBuf>,
        client_name: impl Into<String>,
        make_host: HostConfigFactory,
    ) -> Result<Self, AcquireError> {
        let state_dir = state_dir.into();
        let client_name = client_name.into();
        let role = acquire_role(&identity, &state_dir, &client_name, &make_host).await?;
        Ok(Self {
            inner: Arc::new(SessionInner {
                identity,
                state_dir,
                client_name,
                make_host,
                role: Mutex::new(role),
            }),
        })
    }

    /// Whether this session currently holds leadership.
    pub async fn is_leader(&self) -> bool {
        matches!(&*self.inner.role.lock().await, Role::Leader { .. })
    }

    /// Orderly exit: a leader session runs the host's stop-accept → drain →
    /// close → release-discovery shutdown (followers re-race); a follower
    /// just drops its connection.
    pub async fn shutdown(self, grace: Duration) {
        let role = {
            let mut guard = self.inner.role.lock().await;
            std::mem::replace(&mut *guard, Role::Closed)
        };
        if let Role::Leader { host, .. } = role {
            host.shutdown(grace).await;
        }
    }

    /// The current executable snapshot.
    async fn snapshot(&self) -> Result<Exec, CallError> {
        match &*self.inner.role.lock().await {
            Role::Leader { config, .. } => Ok(Exec::Local(Arc::clone(&config.dispatch))),
            Role::Follower { client } => Ok(Exec::Remote(Arc::clone(client))),
            Role::Closed => Err(CallError::Closed),
        }
    }

    /// Re-race the context after `stale`'s connection died. Serialized on
    /// the role lock; if another caller already re-raced (the role no longer
    /// holds `stale`), its outcome is adopted instead of racing again — a
    /// promoted session must never end up following itself.
    ///
    /// Returns whether THIS process now leads.
    async fn reacquire(&self, stale: &Arc<ContextClient>) -> Result<bool, CallError> {
        let mut role = self.inner.role.lock().await;
        match &*role {
            Role::Leader { .. } => return Ok(true),
            Role::Closed => return Err(CallError::Closed),
            Role::Follower { client } => {
                if !Arc::ptr_eq(client, stale) {
                    // Someone already reconnected us — adopt their outcome.
                    return Ok(false);
                }
            }
        }
        let fresh = acquire_role(
            &self.inner.identity,
            &self.inner.state_dir,
            &self.inner.client_name,
            &self.inner.make_host,
        )
        .await
        .map_err(CallError::Acquire)?;
        let promoted = matches!(fresh, Role::Leader { .. });
        *role = fresh;
        Ok(promoted)
    }

    /// One attempt against an executable snapshot. `Err(client)` is the
    /// connection-lost case, carrying the client whose connection died.
    #[allow(clippy::type_complexity)]
    async fn attempt(
        &self,
        exec: Exec,
        tool: &str,
        args: &serde_json::Map<String, Value>,
    ) -> Result<Result<Value, CallError>, Arc<ContextClient>> {
        match exec {
            Exec::Local(dispatch) => Ok(dispatch(None, tool.to_string(), args.clone())
                .await
                .map_err(CallError::Tool)),
            Exec::Remote(client) => match client.call(tool, args.clone()).await {
                Ok(value) => Ok(Ok(value)),
                Err(CallFailure::Tool(msg)) => Ok(Err(CallError::Tool(msg))),
                Err(CallFailure::ConnectionLost) => Err(client),
            },
        }
    }
}

/// Run one acquisition, wrapping the outcome as a [`Role`] (keeping the
/// promoted config on the leader path — the factory runs only there).
async fn acquire_role(
    identity: &ContextIdentity,
    state_dir: &std::path::Path,
    client_name: &str,
    make_host: &HostConfigFactory,
) -> Result<Role, AcquireError> {
    let mut kept: Option<HostConfig> = None;
    let acquired = acquire(identity, state_dir, client_name, || {
        let config = (make_host)();
        kept = Some(config.clone());
        config
    })
    .await?;
    Ok(match acquired {
        Acquired::Leader(host) => Role::Leader {
            host,
            config: kept.expect("the factory ran on the leader path"),
        },
        Acquired::Follower(client) => Role::Follower {
            client: Arc::new(client),
        },
    })
}

#[async_trait]
impl FleetContext for ContextSession {
    async fn call(
        &self,
        tool: &str,
        args: serde_json::Map<String, Value>,
        idempotent: bool,
    ) -> Result<Value, CallError> {
        let exec = self.snapshot().await?;
        let stale = match self.attempt(exec, tool, &args).await {
            Ok(outcome) => return outcome,
            Err(stale) => stale,
        };
        // The leader died under our call: re-race — promote or reconnect —
        // then retry exactly once, reads only. A mutation surfaces the
        // structured failover (the registry, not a retry, says what
        // actually launched).
        let promoted = self.reacquire(&stale).await?;
        if !idempotent {
            return Err(CallError::Failover {
                retried: false,
                promoted,
            });
        }
        let exec = self.snapshot().await?;
        match self.attempt(exec, tool, &args).await {
            Ok(outcome) => outcome,
            Err(_) => Err(CallError::Failover {
                retried: true,
                promoted,
            }),
        }
    }

    /// One resilient subscription per session: while following, the remote
    /// stream feeds it; when the stream closes (leader death), the session
    /// re-races and resumes — from its OWN broadcast if it promoted, from
    /// the new leader's stream otherwise. The first attachment completes
    /// BEFORE this returns, so events published afterwards are guaranteed to
    /// flow. The receiver closes only when the session does (or the
    /// subscriber hangs up).
    async fn subscribe(&self) -> Result<mpsc::Receiver<Value>, CallError> {
        let mut attached = self.attach().await?;
        let (tx, rx) = mpsc::channel::<Value>(256);
        let session = self.clone();
        tokio::spawn(async move {
            loop {
                match attached {
                    Attached::Local(mut events) => loop {
                        match events.recv().await {
                            Ok(event) => {
                                if tx.send(event).await.is_err() {
                                    return;
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => {}
                            // The leader's event sender is gone: the host is
                            // being torn down — the stream ends with it.
                            Err(broadcast::error::RecvError::Closed) => return,
                        }
                    },
                    Attached::Remote(client, mut stream) => {
                        while let Some(event) = stream.recv().await {
                            if tx.send(event).await.is_err() {
                                return;
                            }
                        }
                        // The remote stream closed: the leader is gone —
                        // re-race and resume.
                        if session.reacquire(&client).await.is_err() {
                            return;
                        }
                    }
                }
                attached = match session.attach().await {
                    Ok(attached) => attached,
                    Err(_) => return,
                };
            }
        });
        Ok(rx)
    }
}

/// A live event source the subscription loop is currently draining.
enum Attached {
    /// This session leads: its own activity broadcast.
    Local(broadcast::Receiver<Value>),
    /// Following: the leader's event stream, with the connection it rides.
    Remote(Arc<ContextClient>, mpsc::Receiver<Value>),
}

impl ContextSession {
    /// Attach to the current role's event source, re-racing through dead
    /// connections until one attaches (or the session closes / acquisition
    /// fails outright).
    async fn attach(&self) -> Result<Attached, CallError> {
        loop {
            let client = match &*self.inner.role.lock().await {
                Role::Leader { config, .. } => {
                    return Ok(Attached::Local(config.events.subscribe()));
                }
                Role::Follower { client } => Arc::clone(client),
                Role::Closed => return Err(CallError::Closed),
            };
            match client.subscribe().await {
                Ok(stream) => return Ok(Attached::Remote(client, stream)),
                Err(_) => {
                    // The connection died before (or during) the subscribe:
                    // re-race, then try the fresh role.
                    self.reacquire(&client).await?;
                    tokio::time::sleep(RESUBSCRIBE_BACKOFF).await;
                }
            }
        }
    }
}
