//! mandala-context — the shared fleet execution context substrate (OpenSpec
//! change `mandala-native-tui`, capability `fleet-context`; spike task 1.1).
//!
//! One execution context per canonical flake path. Leadership is acquired by
//! **binding the loopback coordination endpoint — the bind IS the lock**:
//! kernel-arbitrated, atomic, self-cleaning on process death, no lockfile
//! protocol anywhere. The discovery file
//! (`<state_dir>/mcp/contexts/<key>.json`, 0600) is metadata only —
//! `{url, token, pid, flake}` — and liveness is judged solely by connecting;
//! pids are advisory (recycled-pid caveat carried over from phase 1).
//!
//! [`acquire`] is the one entry point: probe the discovery url first (it is
//! authoritative for clients — the leader may sit on a walked port), then
//! walk the identity's deterministic port sequence, binding where free and
//! probing where occupied. A live listener whose rejection reports a
//! *different* flake is another context squatting our derived port — step to
//! the next port; one reporting *our* flake is our leader (join it, re-reading
//! the discovery token if ours lost the mint race).

pub mod client;
pub mod discovery;
pub mod fleet;
pub mod host;
pub mod identity;
pub mod protocol;

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use tokio::net::TcpListener;

pub use client::{CallFailure, ConnectError, ContextClient};
pub use discovery::{Discovery, mint_token};
pub use fleet::{CallError, ContextSession, FleetContext, HostConfigFactory, LocalContext};
pub use host::{Dispatch, DispatchFuture, HostConfig, RunningHost};
pub use identity::ContextIdentity;

/// How the process joined the context.
pub enum Acquired {
    /// We bound the endpoint: we ARE the context now. The discovery file has
    /// been (re)written around the actual bound port.
    Leader(RunningHost),
    /// A live leader answered: this is a connected, authenticated follower
    /// handle.
    Follower(ContextClient),
}

/// Why the context could not be acquired or joined.
#[derive(Debug)]
pub enum AcquireError {
    /// Filesystem/socket failure outside the protocol.
    Io(io::Error),
    /// Every port in the range is held by something that is not our context.
    PortsExhausted,
    /// A live leader for our flake rejected our token and the discovery file
    /// never yielded a working one.
    TokenUnavailable,
}

impl From<io::Error> for AcquireError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "context acquire failed: {e}"),
            Self::PortsExhausted => write!(f, "no free port in the context range"),
            Self::TokenUnavailable => {
                write!(
                    f,
                    "live leader rejected our token and discovery never refreshed"
                )
            }
        }
    }
}

impl std::error::Error for AcquireError {}

/// How long a mint-race loser polls the discovery file for the winner's
/// token before giving up.
const TOKEN_RACE_WINDOW: Duration = Duration::from_secs(2);
const TOKEN_RACE_POLL: Duration = Duration::from_millis(50);

/// Acquire the context for `identity`: join the live leader if one answers,
/// else bind-as-lock and become it.
///
/// `client_name` is this process's identity in hellos (labels its calls in
/// the leader's activity stream). `config` is a factory invoked ONLY on the
/// leader path — a joiner never pays for (or observes) leader-side setup.
///
/// # Errors
/// [`AcquireError`] — I/O failures, an exhausted port range, or an
/// unresolvable token race.
pub async fn acquire(
    identity: &ContextIdentity,
    state_dir: &Path,
    client_name: &str,
    config: impl FnOnce() -> HostConfig,
) -> Result<Acquired, AcquireError> {
    let mut config = Some(config);
    // Reuse the context's token when a discovery file for OUR flake exists —
    // stable across leader restarts (spec: token mint/reuse). A stale or
    // foreign file just means we mint.
    let existing = discovery::read(state_dir, identity.key())
        .filter(|d| d.flake == identity.flake() && !d.token.is_empty());
    let token = existing
        .as_ref()
        .map_or_else(mint_token, |d| d.token.clone());

    // The discovery url is authoritative for clients: the leader may sit on
    // a walked (non-derived) port. Probe it first; a dead endpoint here is
    // exactly the stale-discovery case — fall through and claim.
    if let Some(addr) = existing.as_ref().and_then(Discovery::addr) {
        match probe(addr, &token, client_name, identity.flake()).await {
            Probe::OurLeader(client) => return Ok(Acquired::Follower(client)),
            Probe::TokenMismatch | Probe::Foreign | Probe::Dead => {}
        }
    }

    for port in identity.ports() {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        match TcpListener::bind(addr).await {
            Ok(listener) => {
                let config = config.take().expect("the leader path runs at most once")();
                let host = RunningHost::start(
                    listener,
                    token.clone(),
                    identity.flake().to_string(),
                    config,
                )
                .published_at(state_dir, identity.key());
                discovery::write(
                    state_dir,
                    identity.key(),
                    &Discovery {
                        url: host.url(),
                        token,
                        pid: std::process::id(),
                        flake: identity.flake().to_string(),
                    },
                )?;
                return Ok(Acquired::Leader(host));
            }
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                match probe(addr, &token, client_name, identity.flake()).await {
                    Probe::OurLeader(client) => return Ok(Acquired::Follower(client)),
                    Probe::TokenMismatch => {
                        // Our context's live leader, our token stale: we lost
                        // a mint race (concurrent first claims) or hold a
                        // rotated token. The winner writes discovery right
                        // after binding — poll it for the good token.
                        return join_with_refreshed_token(
                            addr,
                            state_dir,
                            identity,
                            client_name,
                            &token,
                        )
                        .await;
                    }
                    // Another context's endpoint or a foreign service on our
                    // derived port: deterministic step to the next port.
                    Probe::Foreign | Probe::Dead => {}
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(AcquireError::PortsExhausted)
}

/// What answered a connection probe.
enum Probe {
    /// Our context's live leader — already helloed, ready to use.
    OurLeader(ContextClient),
    /// A live context for our flake whose token is not ours.
    TokenMismatch,
    /// A different context, or something that does not speak the protocol.
    Foreign,
    /// Nothing accepted the connection.
    Dead,
}

/// Connect + hello to classify whatever listens at `addr`.
async fn probe(addr: SocketAddr, token: &str, client_name: &str, flake: &str) -> Probe {
    match ContextClient::connect(addr, token, client_name, flake).await {
        Ok(client) if client.server_flake == flake => Probe::OurLeader(client),
        // A welcome from a different flake means our token somehow matched a
        // foreign context — treat as foreign, never join it.
        Ok(_) => Probe::Foreign,
        Err(ConnectError::Unauthorized { server_flake }) => {
            if server_flake.as_deref() == Some(flake) {
                Probe::TokenMismatch
            } else {
                Probe::Foreign
            }
        }
        Err(ConnectError::NotAContext(_)) => Probe::Foreign,
        Err(ConnectError::Io(_)) => Probe::Dead,
    }
}

/// A live leader for our flake rejected `stale`: poll discovery until the
/// winner's write lands, then hello with the fresh token.
async fn join_with_refreshed_token(
    addr: SocketAddr,
    state_dir: &Path,
    identity: &ContextIdentity,
    client_name: &str,
    stale: &str,
) -> Result<Acquired, AcquireError> {
    let deadline = tokio::time::Instant::now() + TOKEN_RACE_WINDOW;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(TOKEN_RACE_POLL).await;
        let Some(d) = discovery::read(state_dir, identity.key())
            .filter(|d| d.flake == identity.flake() && !d.token.is_empty() && d.token != stale)
        else {
            continue;
        };
        match ContextClient::connect(addr, &d.token, client_name, identity.flake()).await {
            Ok(client) if client.server_flake == identity.flake() => {
                return Ok(Acquired::Follower(client));
            }
            // Still racing (another rotation, or the leader died): keep
            // polling until the window closes.
            _ => {}
        }
    }
    Err(AcquireError::TokenUnavailable)
}
