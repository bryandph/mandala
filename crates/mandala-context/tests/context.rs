//! Context-endpoint spike gates (OpenSpec change `mandala-native-tui`, task
//! 1.1): bind-as-lock, the promotion race, stale discovery, the port
//! collision walk, and protocol v1 over real loopback sockets.
//!
//! Every test binds REAL `TcpListener`s — kernel arbitration is the point.
//! Port-derived tests use small injected ranges at distinct bases so they
//! don't contend with each other; assertions are relational (winner counts,
//! sequence membership, discovery agreement), never absolute port numbers,
//! so an unrelated local service squatting a test port cannot flake them —
//! the walk is exactly what handles that.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;

use mandala_context::{
    Acquired, ContextClient, ContextIdentity, Discovery, HostConfig, RunningHost, acquire,
    discovery,
};

/// A per-test scratch tree: `flake/` (the canonicalizable checkout stand-in)
/// and `state/` (the isolated mandala state dir).
fn scratch(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!(
        "mandala-context-spike-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let flake = base.join("flake");
    let state = base.join("state");
    std::fs::create_dir_all(&flake).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    (flake, state)
}

/// A dispatch that echoes `{tool, origin, args}` after an optional
/// `sleep_ms` delay, and (when given a sender) publishes one activity event
/// before settling. Wire calls carry the hello client as origin; local
/// calls echo `"local"`.
fn echo_dispatch(events: Option<broadcast::Sender<Value>>) -> mandala_context::Dispatch {
    Arc::new(move |origin, tool, args| {
        let events = events.clone();
        Box::pin(async move {
            let origin = origin.unwrap_or_else(|| "local".to_string());
            if let Some(ms) = args.get("sleep_ms").and_then(Value::as_u64) {
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
            if let Some(events) = &events {
                let _ = events.send(json!({
                    "tool": tool, "origin": origin, "status": "ok",
                }));
            }
            Ok(json!({"tool": tool, "origin": origin, "args": args}))
        })
    })
}

fn config_with(events: broadcast::Sender<Value>, heartbeat: Duration) -> HostConfig {
    HostConfig {
        dispatch: echo_dispatch(Some(events.clone())),
        events,
        heartbeat_interval: heartbeat,
    }
}

fn config() -> HostConfig {
    let (events, _) = broadcast::channel(64);
    config_with(events, HostConfig::DEFAULT_HEARTBEAT)
}

// ---- (b)+(c) bind-as-lock + the promotion race ------------------------------

/// N concurrent acquires for one context: the kernel hands the bind to
/// exactly one; every loser completes hello/welcome against the winner —
/// including the losers that minted their own token before the winner's
/// discovery write landed (the token-race path).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn promotion_race_has_exactly_one_winner() {
    let (flake, state) = scratch("race");
    let identity = ContextIdentity::with_port_range(&flake, 28710, 4).unwrap();

    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..8 {
        let identity = identity.clone();
        let state = state.clone();
        tasks.spawn(async move { acquire(&identity, &state, &format!("racer-{i}"), config).await });
    }
    let mut leaders = Vec::new();
    let mut followers = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined.unwrap().expect("every racer acquires or joins") {
            Acquired::Leader(host) => leaders.push(host),
            Acquired::Follower(client) => followers.push(client),
        }
    }
    assert_eq!(leaders.len(), 1, "the bind arbitrates exactly one leader");
    assert_eq!(followers.len(), 7);

    let leader = &leaders[0];
    assert!(identity.ports().any(|p| p == leader.port()));

    // Discovery records the actual bound port and the winner's identity.
    let d = discovery::read(&state, identity.key()).expect("discovery written");
    assert_eq!(d.url, leader.url());
    assert_eq!(d.pid, std::process::id());
    assert_eq!(d.flake, identity.flake());

    // Every follower is authenticated against the SAME live endpoint and can
    // execute a call through it.
    for (i, client) in followers.iter().enumerate() {
        assert_eq!(client.server_flake, identity.flake());
        assert_eq!(client.server_pid, std::process::id());
        let result = client
            .call("echo", json!({"i": i}).as_object().cloned().unwrap())
            .await
            .expect("follower call succeeds");
        assert_eq!(result["args"]["i"], json!(i));
    }
}

// ---- (d) stale discovery ----------------------------------------------------

/// A discovery file pointing at a dead endpoint (nothing listening; bogus,
/// possibly recycled pid) never blocks a claim: the next process binds,
/// becomes leader, rewrites the file — and REUSES the context token, so the
/// token is stable across leader restarts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_discovery_is_reclaimed_without_cleanup() {
    let (flake, state) = scratch("stale");
    let identity = ContextIdentity::with_port_range(&flake, 28720, 4).unwrap();

    // A provably-dead loopback port: bind ephemeral, note it, release it.
    let dead_port = {
        let l = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        l.local_addr().unwrap().port()
    };
    let stale_token = discovery::mint_token();
    discovery::write(
        &state,
        identity.key(),
        &Discovery {
            url: format!("tcp://127.0.0.1:{dead_port}"),
            token: stale_token.clone(),
            pid: 999_999_999, // advisory only — a recycled pid must not block
            flake: identity.flake().to_string(),
        },
    )
    .unwrap();

    let acquired = acquire(&identity, &state, "reclaimer", config)
        .await
        .expect("stale discovery must not block the claim");
    let Acquired::Leader(host) = acquired else {
        panic!("nothing listens — the claimant must become leader");
    };

    let d = discovery::read(&state, identity.key()).unwrap();
    assert_eq!(d.url, host.url(), "rewritten around the live endpoint");
    assert_eq!(d.pid, std::process::id());
    assert_eq!(d.token, stale_token, "token reused: stable across restarts");
    assert!(identity.ports().any(|p| p == host.port()));

    // And the rewritten discovery actually serves joiners.
    let joiner = acquire(&identity, &state, "joiner", config).await.unwrap();
    assert!(matches!(joiner, Acquired::Follower(_)));
}

// ---- (a) port collision between two contexts --------------------------------

/// Temp checkouts until two DIFFERENT canonical paths derive the same port
/// within `range` — the deliberate collision.
fn colliding_identities(base: u16, range: u16) -> (ContextIdentity, ContextIdentity) {
    let (flake_a, _) = scratch("collide-a");
    let a = ContextIdentity::with_port_range(&flake_a, base, range).unwrap();
    for i in 0..10_000 {
        let (flake_b, _) = scratch(&format!("collide-b{i}"));
        let b = ContextIdentity::with_port_range(&flake_b, base, range).unwrap();
        if b.derived_port() == a.derived_port() && b.flake() != a.flake() {
            return (a, b);
        }
    }
    panic!("no collision found in 10k tries — derivation is broken");
}

/// Two contexts whose flake paths derive the SAME port: the first binds it;
/// the second probes, learns from the structured rejection that a DIFFERENT
/// flake answers, and deterministically steps to the next port. Both end up
/// live, isolated, and discoverable at their recorded (authoritative) urls.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn derived_port_collision_walks_and_converges() {
    let (a, b) = colliding_identities(28730, 8);
    let (_, state_a) = scratch("collide-state-a");
    let (_, state_b) = scratch("collide-state-b");

    let leader_a = match acquire(&a, &state_a, "ctx-a", config).await.unwrap() {
        Acquired::Leader(host) => host,
        Acquired::Follower(_) => panic!("first context must lead"),
    };
    let leader_b = match acquire(&b, &state_b, "ctx-b", config).await.unwrap() {
        Acquired::Leader(host) => host,
        Acquired::Follower(_) => {
            panic!("colliding context must walk to its own bind, not join a foreign flake")
        }
    };

    assert_ne!(leader_a.port(), leader_b.port());
    assert!(b.ports().any(|p| p == leader_b.port()), "still in range");

    // The rejection that drove the walk: hello at A's port with B's token
    // reports A's flake — that's how B knew to move on.
    let da = discovery::read(&state_a, a.key()).unwrap();
    let db = discovery::read(&state_b, b.key()).unwrap();
    let Err(err) = ContextClient::connect(da.addr().unwrap(), &db.token, "probe", b.flake()).await
    else {
        panic!("foreign token must be rejected");
    };
    match err {
        mandala_context::ConnectError::Unauthorized { server_flake } => {
            assert_eq!(server_flake.as_deref(), Some(a.flake()));
        }
        other => panic!("expected structured unauthorized, got {other}"),
    }

    // Convergence: a joiner for B follows B's discovery to the walked port.
    let joiner = acquire(&b, &state_b, "b-joiner", config).await.unwrap();
    let Acquired::Follower(client) = joiner else {
        panic!("B's context is live — joiners must follow, not re-lead");
    };
    assert_eq!(client.server_flake, b.flake());
    assert_eq!(db.url, leader_b.url(), "discovery records the walked port");
}

// ---- (e) protocol v1 --------------------------------------------------------

/// Bind an ephemeral port and serve the protocol on it (pure protocol tests
/// need no derived ports).
async fn test_host(
    events: broadcast::Sender<Value>,
    heartbeat: Duration,
) -> (RunningHost, SocketAddr, String) {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let token = discovery::mint_token();
    let host = RunningHost::start(
        listener,
        token.clone(),
        "/spike/flake".to_string(),
        config_with(events, heartbeat),
    );
    (host, addr, token)
}

/// A wrong token gets exactly one structured error frame — carrying the
/// server's flake (the collision probe's signal) — and then the close;
/// nothing else is ever served.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_token_gets_structured_error_then_close() {
    let (events, _) = broadcast::channel(8);
    let (_host, addr, _token) = test_host(events, HostConfig::DEFAULT_HEARTBEAT).await;

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            b"{\"type\":\"hello\",\"v\":1,\"token\":\"wrong\",\"client\":\"x\",\"flake\":\"/other\"}\n",
        )
        .await
        .unwrap();
    let (read_half, _write_half) = stream.split();
    let mut lines = BufReader::new(read_half).lines();

    let first = lines.next_line().await.unwrap().expect("one error frame");
    let frame: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(frame["type"], "error");
    assert_eq!(frame["error"], "unauthorized");
    assert_eq!(frame["flake"], "/spike/flake");

    let second = lines.next_line().await.unwrap();
    assert_eq!(second, None, "nothing beyond the auth failure; closed");
}

/// hello/welcome, call/result, subscribe + event push, and ping — one
/// connection each, over the injected echo dispatch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn call_subscribe_ping_roundtrips() {
    let (events, _) = broadcast::channel(64);
    let (_host, addr, token) = test_host(events, HostConfig::DEFAULT_HEARTBEAT).await;

    let observer = ContextClient::connect(addr, &token, "observer", "/spike/flake")
        .await
        .unwrap();
    assert_eq!(observer.server_flake, "/spike/flake");
    assert_eq!(observer.server_pid, std::process::id());
    let mut stream = observer.subscribe().await.unwrap();

    let caller = ContextClient::connect(addr, &token, "caller", "/spike/flake")
        .await
        .unwrap();
    assert!(caller.ping().await, "pong before any call");

    let result = caller
        .call("echo", json!({"x": 1}).as_object().cloned().unwrap())
        .await
        .unwrap();
    assert_eq!(
        result,
        json!({"tool": "echo", "origin": "caller", "args": {"x": 1}})
    );

    // The dispatch published one event; the subscribed observer receives it,
    // origin-labeled with the CALLER's hello identity.
    let event = tokio::time::timeout(Duration::from_secs(5), stream.recv())
        .await
        .expect("event within 5s")
        .expect("stream open");
    assert_eq!(
        event,
        json!({"tool": "echo", "origin": "caller", "status": "ok"})
    );
}

/// A long-blocking call's connection stays observably alive: heartbeat
/// frames flow while the call is in flight — and only then.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heartbeats_cover_a_blocking_call() {
    let (events, _) = broadcast::channel(8);
    let (_host, addr, token) = test_host(events, Duration::from_millis(25)).await;

    let client = ContextClient::connect(addr, &token, "waiter", "/spike/flake")
        .await
        .unwrap();

    // Idle: no heartbeats (idle liveness is ping's job).
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(client.heartbeats_seen(), 0, "no heartbeats while idle");

    // In flight for ~400ms at a 25ms cadence: the connection proves itself.
    let result = client
        .call(
            "slow",
            json!({"sleep_ms": 400}).as_object().cloned().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result["tool"], "slow");
    let seen = client.heartbeats_seen();
    assert!(seen >= 3, "expected heartbeats during the call, saw {seen}");
}
