//! Leadership lifecycle gates (OpenSpec change `mandala-native-tui`, tasks
//! 2.1 + 2.2): explicit release, token rotation, pid-advisory liveness, the
//! orderly stop-accept → drain → close → release shutdown (the Python
//! quit-crash lesson), prompt dead-subscriber reaping, and the non-blocking
//! activity fan-out.
//!
//! Same discipline as `tests/context.rs`: real loopback sockets, small
//! injected port ranges at distinct bases, relational assertions.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::broadcast;

use mandala_context::{
    Acquired, ConnectError, ContextClient, ContextIdentity, Discovery, HostConfig, acquire,
    discovery,
};

/// A per-test scratch tree: `flake/` (the canonicalizable checkout stand-in)
/// and `state/` (the isolated mandala state dir).
fn scratch(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!(
        "mandala-context-lifecycle-{tag}-{}-{}",
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

/// An echo dispatch honoring `sleep_ms` (the in-flight-call knob).
fn echo_dispatch() -> mandala_context::Dispatch {
    Arc::new(move |origin, tool, args| {
        Box::pin(async move {
            if let Some(ms) = args.get("sleep_ms").and_then(Value::as_u64) {
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
            Ok(json!({"tool": tool, "origin": origin, "args": args}))
        })
    })
}

fn config_with(events: broadcast::Sender<Value>) -> HostConfig {
    HostConfig {
        dispatch: echo_dispatch(),
        events,
        heartbeat_interval: HostConfig::DEFAULT_HEARTBEAT,
    }
}

fn config() -> HostConfig {
    let (events, _) = broadcast::channel(64);
    config_with(events)
}

// ---- 2.1: explicit release --------------------------------------------------

/// Orderly shutdown releases everything: the discovery claim is removed (no
/// stale metadata after a CLEAN exit), the port refuses connects, and the
/// next acquire simply leads again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clean_shutdown_releases_discovery_and_port() {
    let (flake, state) = scratch("release");
    let identity = ContextIdentity::with_port_range(&flake, 28750, 4).unwrap();

    let host = match acquire(&identity, &state, "leader", config).await.unwrap() {
        Acquired::Leader(host) => host,
        Acquired::Follower(_) => panic!("no prior context — must lead"),
    };
    let addr = discovery::read(&state, identity.key())
        .unwrap()
        .addr()
        .unwrap();

    host.shutdown(Duration::from_secs(2)).await;

    assert_eq!(
        discovery::read(&state, identity.key()),
        None,
        "a clean release removes the discovery claim"
    );
    assert!(
        tokio::net::TcpStream::connect(addr).await.is_err(),
        "the released port must refuse connects"
    );

    // The context is simply claimable again — a fresh mint, no manual
    // cleanup.
    let again = acquire(&identity, &state, "leader-2", config)
        .await
        .unwrap();
    assert!(matches!(again, Acquired::Leader(_)));
}

/// The release is GUARDED: if a racer already claimed the context between
/// the drain and the release, its discovery file is never clobbered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn release_never_clobbers_a_newer_claim() {
    let (flake, state) = scratch("guarded");
    let identity = ContextIdentity::with_port_range(&flake, 28755, 4).unwrap();

    let host = match acquire(&identity, &state, "leader", config).await.unwrap() {
        Acquired::Leader(host) => host,
        Acquired::Follower(_) => panic!("must lead"),
    };

    // Simulate the racer that claimed while we were draining: a discovery
    // file that is no longer ours.
    let usurper = Discovery {
        url: "tcp://127.0.0.1:1".to_string(),
        token: discovery::mint_token(),
        pid: 999_999_999,
        flake: identity.flake().to_string(),
    };
    discovery::write(&state, identity.key(), &usurper).unwrap();

    host.shutdown(Duration::from_secs(1)).await;
    assert_eq!(
        discovery::read(&state, identity.key()),
        Some(usurper),
        "shutdown must only remove its OWN claim"
    );
}

// ---- 2.1: token rotation ----------------------------------------------------

/// Rotation semantics of record: the fresh token is minted, discovery is
/// rewritten around it, old-token holders are cut off at their NEXT connect
/// — while already-authenticated connections live on and complete calls.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn token_rotation_cuts_old_connects_not_live_connections() {
    let (flake, state) = scratch("rotate");
    let identity = ContextIdentity::with_port_range(&flake, 28760, 4).unwrap();

    let host = match acquire(&identity, &state, "leader", config).await.unwrap() {
        Acquired::Leader(host) => host,
        Acquired::Follower(_) => panic!("must lead"),
    };
    let before = discovery::read(&state, identity.key()).unwrap();
    let addr = before.addr().unwrap();

    // Authenticated BEFORE the rotation — must survive it.
    let veteran = ContextClient::connect(addr, &before.token, "veteran", identity.flake())
        .await
        .unwrap();

    let fresh = host.rotate_token().unwrap();
    assert_ne!(fresh, before.token);

    // Discovery is rewritten around the fresh token; everything else stands.
    let after = discovery::read(&state, identity.key()).unwrap();
    assert_eq!(after.token, fresh);
    assert_eq!(after.url, before.url);
    assert_eq!(after.pid, before.pid);
    assert_eq!(after.flake, before.flake);

    // Old token: cut off at the next connect, with the structured rejection.
    match ContextClient::connect(addr, &before.token, "stale", identity.flake()).await {
        Err(ConnectError::Unauthorized { server_flake }) => {
            assert_eq!(server_flake.as_deref(), Some(identity.flake()));
        }
        Err(other) => panic!("old token must be unauthorized, got {other}"),
        Ok(_) => panic!("old token must be unauthorized, got a welcome"),
    }

    // Fresh token: welcome.
    let joiner = ContextClient::connect(addr, &fresh, "joiner", identity.flake())
        .await
        .unwrap();
    assert_eq!(joiner.server_flake, identity.flake());

    // The veteran's authenticated connection still completes calls.
    let result = veteran
        .call("echo", json!({"x": 1}).as_object().cloned().unwrap())
        .await
        .expect("in-flight authenticated connections outlive a rotation");
    assert_eq!(result["args"]["x"], json!(1));

    // A later acquire (reading rewritten discovery) joins seamlessly.
    let acquired = acquire(&identity, &state, "late-joiner", config)
        .await
        .unwrap();
    assert!(matches!(acquired, Acquired::Follower(_)));
}

// ---- 2.1: connect-probe is the only liveness judgement ----------------------

/// A stale discovery file recording a LIVE pid (this very process — the
/// recycled-pid case in its sharpest form) still never blocks the claim:
/// liveness is judged by connecting, pids are advisory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_pid_in_stale_discovery_does_not_block_claim() {
    let (flake, state) = scratch("livepid");
    let identity = ContextIdentity::with_port_range(&flake, 28765, 4).unwrap();

    let dead_port = {
        let l = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        l.local_addr().unwrap().port()
    };
    discovery::write(
        &state,
        identity.key(),
        &Discovery {
            url: format!("tcp://127.0.0.1:{dead_port}"),
            token: discovery::mint_token(),
            pid: std::process::id(), // alive and well — and irrelevant
            flake: identity.flake().to_string(),
        },
    )
    .unwrap();

    let acquired = acquire(&identity, &state, "claimant", config)
        .await
        .expect("a live pid on a dead endpoint must not block");
    assert!(matches!(acquired, Acquired::Leader(_)));
}

// ---- 2.2: orderly shutdown --------------------------------------------------

/// The load-bearing sequence: DURING shutdown a new connection is refused
/// while an in-flight call still completes and its result reaches the
/// client; AFTER shutdown the port is closed outright.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_drains_in_flight_and_refuses_new() {
    let (flake, state) = scratch("drain");
    let identity = ContextIdentity::with_port_range(&flake, 28770, 4).unwrap();

    let host = match acquire(&identity, &state, "leader", config).await.unwrap() {
        Acquired::Leader(host) => host,
        Acquired::Follower(_) => panic!("must lead"),
    };
    let d = discovery::read(&state, identity.key()).unwrap();
    let addr = d.addr().unwrap();

    let caller = ContextClient::connect(addr, &d.token, "caller", identity.flake())
        .await
        .unwrap();
    let in_flight = tokio::spawn(async move {
        caller
            .call(
                "slow",
                json!({"sleep_ms": 600}).as_object().cloned().unwrap(),
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let token = d.token.clone();
    let flake_path = identity.flake().to_string();
    let shutdown = tokio::spawn(async move { host.shutdown(Duration::from_secs(5)).await });
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Mid-shutdown: the endpoint refuses new clients (accepted, then closed
    // before the welcome — a probe reads "not a context").
    assert!(
        ContextClient::connect(addr, &token, "latecomer", &flake_path)
            .await
            .is_err(),
        "a new connection during shutdown must be refused"
    );

    // …while the in-flight call drains to completion and its result crosses
    // the wire.
    let result = in_flight
        .await
        .unwrap()
        .expect("the in-flight call must complete during the drain");
    assert_eq!(result["tool"], "slow");

    shutdown.await.unwrap();
    assert!(
        tokio::net::TcpStream::connect(addr).await.is_err(),
        "after shutdown the port must refuse connects"
    );
    assert_eq!(discovery::read(&state, identity.key()), None);
}

// ---- 2.2: subscriber hygiene ------------------------------------------------

/// A subscriber whose process vanished is reaped at read-EOF — promptly,
/// not at the next event send: its broadcast receiver is released within a
/// beat of the connection closing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dead_subscriber_is_reaped_promptly() {
    let (flake, state) = scratch("reap");
    let identity = ContextIdentity::with_port_range(&flake, 28775, 4).unwrap();

    let (events, _) = broadcast::channel::<Value>(64);
    let events_probe = events.clone();
    let _host = match acquire(&identity, &state, "leader", move || config_with(events))
        .await
        .unwrap()
    {
        Acquired::Leader(host) => host,
        Acquired::Follower(_) => panic!("must lead"),
    };
    let d = discovery::read(&state, identity.key()).unwrap();

    let subscriber = ContextClient::connect(d.addr().unwrap(), &d.token, "sub", identity.flake())
        .await
        .unwrap();
    let stream = subscriber.subscribe().await.unwrap();
    assert_eq!(events_probe.receiver_count(), 1, "one live subscription");

    // The peer dies (client dropped → connection closed). NO event is ever
    // published — the reap must come from the read-EOF, not a failed send.
    drop(stream);
    drop(subscriber);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while events_probe.receiver_count() > 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        events_probe.receiver_count(),
        0,
        "the dead subscriber's broadcast receiver must be released promptly"
    );
}

/// A subscriber that never reads cannot stall the call path: with its
/// buffers saturated by an event flood, a concurrent tool call still
/// settles immediately (lagged subscribers lose events, never block them).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_subscriber_never_stalls_calls() {
    let (flake, state) = scratch("slowsub");
    let identity = ContextIdentity::with_port_range(&flake, 28780, 4).unwrap();

    let (events, _) = broadcast::channel::<Value>(64);
    let flood = events.clone();
    let _host = match acquire(&identity, &state, "leader", move || config_with(events))
        .await
        .unwrap()
    {
        Acquired::Leader(host) => host,
        Acquired::Follower(_) => panic!("must lead"),
    };
    let d = discovery::read(&state, identity.key()).unwrap();
    let addr = d.addr().unwrap();

    // A raw socket that hellos, subscribes — and then never reads again.
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut stalled = tokio::net::TcpStream::connect(addr).await.unwrap();
    let hello = format!(
        "{{\"type\":\"hello\",\"v\":1,\"token\":\"{}\",\"client\":\"stalled\",\"flake\":\"{}\"}}\n",
        d.token,
        identity.flake()
    );
    stalled.write_all(hello.as_bytes()).await.unwrap();
    let (read_half, mut write_half) = stalled.split();
    let mut lines = BufReader::new(read_half).lines();
    let welcome = lines.next_line().await.unwrap().unwrap();
    assert!(welcome.contains("welcome"), "got: {welcome}");
    write_half
        .write_all(b"{\"type\":\"subscribe\"}\n")
        .await
        .unwrap();
    // From here on: no reads. Saturate the fan-out path (conn queue + kernel
    // buffers) with chunky events.
    let payload = "x".repeat(1024);
    for i in 0..20_000 {
        let _ = flood.send(json!({"seq": i, "pad": payload}));
    }

    // A live caller settles a call promptly regardless.
    let caller = ContextClient::connect(addr, &d.token, "caller", identity.flake())
        .await
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        caller.call("echo", json!({"x": 1}).as_object().cloned().unwrap()),
    )
    .await
    .expect("a slow subscriber must never stall a tool call")
    .unwrap();
    assert_eq!(result["args"]["x"], json!(1));
    // Keep the stalled socket alive until here so the flood really did back
    // up against it.
    drop(stalled);
}
