//! LAB-218 diagnosis-only, deliberately failing, opt-in regression.
//!
//! A disappearing foreign port occupant invalidates a contender's completed
//! scan. The contenders then hold TWO listening endpoints for the SAME flake.
//! The sentinel gates protocol replies (not production code) to make the
//! scan/bind interleaving deterministic. No SO_REUSEPORT or shared test roots.
//!
//! Run in a private network namespace on hosts that blackhole unused loopback
//! ports (LAB-206). Build with `cargo test -p mandala-context --test
//! lab218_diagnostic --no-run`, then run that binary in
//! `unshare --user --map-root-user --net` after `ip link set lo up`, with
//! `disappearing_foreign --ignored --nocapture` (expected failure), or
//! `stress_abrupt --ignored --nocapture` (LAB218_TRIALS defaults to 1000).
//! Set TMPDIR, XDG_STATE_HOME and XDG_RUNTIME_DIR to isolated scratch roots.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use mandala_context::{
    Acquired, ContextClient, ContextIdentity, ContextSession, Discovery, FleetContext, HostConfig,
    RunningHost, acquire, discovery,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{Notify, broadcast};

fn config(label: &'static str) -> HostConfig {
    let (events, _) = broadcast::channel(8);
    HostConfig {
        dispatch: Arc::new(move |_, _, _| Box::pin(async move { Ok(json!({"served_by": label})) })),
        events,
        heartbeat_interval: HostConfig::DEFAULT_HEARTBEAT,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "LAB-218: deliberately failing diagnosis-only split-brain regression"]
async fn disappearing_foreign_occupant_invalidates_completed_scan() {
    tokio::time::timeout(Duration::from_secs(10), reproduce())
        .await
        .unwrap();
}

// Same abrupt-drop promotion as failover.rs, with an explicit in-flight
// barrier instead of a 100ms sleep and immediate retry dispatches. Opt-in
// stress only; the environment variable controls the trial count.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "LAB-218: opt-in abrupt promotion stress"]
async fn stress_abrupt_promotion() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let root = std::env::temp_dir().join(format!("lab218-stress-{}", std::process::id()));
    let flake = root.join("flake");
    let state = root.join("state");
    std::fs::create_dir_all(&flake).unwrap();
    let identity = ContextIdentity::with_port_range(&flake, 29810, 4).unwrap();
    let contenders: usize = std::env::var("LAB218_CONTENDERS")
        .unwrap_or_else(|_| "3".into())
        .parse()
        .unwrap();
    assert!(contenders > 0);
    let trials: usize = std::env::var("LAB218_TRIALS")
        .unwrap_or_else(|_| "1000".into())
        .parse()
        .unwrap();
    for trial in 0..trials {
        let entered = Arc::new(AtomicUsize::new(0));
        let dispatch_entered = entered.clone();
        // RunningHost::drop does not cancel dispatched futures. Keep them
        // blocked through failover, then release them to avoid accumulating
        // a pending task per old call across stress trials.
        let finish_old = Arc::new(Notify::new());
        let dispatch_finish = finish_old.clone();
        let mut original = config("original");
        original.dispatch = Arc::new(move |_, _, _| {
            let entered = dispatch_entered.clone();
            let finish = dispatch_finish.clone();
            Box::pin(async move {
                let done = finish.notified();
                tokio::pin!(done);
                done.as_mut().enable();
                entered.fetch_add(1, Ordering::SeqCst);
                done.await;
                Ok(json!({"served_by": "original"}))
            })
        });
        let Acquired::Leader(leader) = acquire(&identity, &state, "original", || original)
            .await
            .unwrap()
        else {
            panic!("original must lead");
        };
        let mut sessions = Vec::new();
        for index in 0..contenders {
            let label = ["s0", "s1", "s2"][index % 3];
            sessions.push(
                ContextSession::acquire(
                    identity.clone(),
                    &state,
                    label,
                    Arc::new(move || config(label)),
                )
                .await
                .unwrap(),
            );
        }
        let mut calls = Vec::new();
        for session in &sessions {
            let session = session.clone();
            calls.push(tokio::spawn(async move {
                session
                    .call("read", serde_json::Map::new(), true)
                    .await
                    .unwrap()
            }));
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while entered.load(Ordering::SeqCst) < sessions.len() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(leader);
        let mut results = Vec::new();
        for call in calls {
            results.push(
                tokio::time::timeout(Duration::from_secs(5), call)
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        finish_old.notify_waiters();
        let mut count = 0;
        for session in &sessions {
            if session.is_leader().await {
                count += 1;
            }
        }
        if count != 1 {
            eprintln!("LAB-218 abrupt trial={trial}, leaders={count}, results={results:?}");
            let d = discovery::read(&state, identity.key()).unwrap();
            for port in identity.ports() {
                if let Ok(client) = ContextClient::connect(
                    (Ipv4Addr::LOCALHOST, port).into(),
                    &d.token,
                    "observer",
                    identity.flake(),
                )
                .await
                {
                    eprintln!(
                        "LAB-218 abrupt live port={port}, result={:?}",
                        client.call("read", serde_json::Map::new()).await
                    );
                }
            }
        }
        assert_eq!(count, 1, "trial={trial}: exactly one abrupt promotion");
        for session in sessions {
            session.shutdown(Duration::ZERO).await;
        }
        if trial % 1000 == 0 {
            eprintln!("LAB-218 completed abrupt trials={}", trial + 1);
        }
    }
    eprintln!("LAB-218 passed {trials} abrupt trials");
}

async fn reproduce() {
    let root = std::env::temp_dir().join(format!(
        "lab218-controlled-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let flake = root.join("flake");
    let state = root.join("state");
    std::fs::create_dir_all(&flake).unwrap();
    let identity = ContextIdentity::with_port_range(&flake, 29800, 3).unwrap();
    let ports: Vec<_> = identity.ports().collect();

    // p0 is a genuinely foreign context; p1 is free; p2 is a foreign
    // protocol sentinel that pauses only the slow contender's scan.
    let first = TcpListener::bind((Ipv4Addr::LOCALHOST, ports[0]))
        .await
        .unwrap();
    let foreign = RunningHost::start(
        first,
        discovery::mint_token(),
        "/foreign/checkout".into(),
        config("foreign"),
    );
    let sentinel = TcpListener::bind((Ipv4Addr::LOCALHOST, ports[2]))
        .await
        .unwrap();
    let scanned = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let scanned_server = scanned.clone();
    let release_server = release.clone();
    let sentinel_task = tokio::spawn(async move {
        loop {
            let (stream, _) = sentinel.accept().await.unwrap();
            let scanned = scanned_server.clone();
            let release = release_server.clone();
            tokio::spawn(async move {
                let (r, mut w) = stream.into_split();
                let mut lines = BufReader::new(r).lines();
                let line = lines.next_line().await.unwrap().unwrap();
                let hello: Value = serde_json::from_str(&line).unwrap();
                if hello["client"] == "slow-contender" {
                    scanned.notify_one();
                    release.notified().await;
                }
                w.write_all(b"{\"type\":\"error\",\"error\":\"unauthorized\",\"flake\":\"/foreign/sentinel\"}\n").await.unwrap();
            });
        }
    });

    // Reuse one token, as in real promotion. Never access the user's state.
    let token = discovery::mint_token();
    discovery::write(
        &state,
        identity.key(),
        &Discovery {
            url: format!("tcp://127.0.0.1:{}", ports[1]),
            token: token.clone(),
            pid: std::process::id(),
            flake: identity.flake().into(),
        },
    )
    .unwrap();
    let slow_id = identity.clone();
    let slow_state = state.clone();
    let slow = tokio::spawn(async move {
        acquire(&slow_id, &slow_state, "slow-contender", || config("slow"))
            .await
            .unwrap()
    });
    scanned.notified().await;

    // Slow has already observed p1 free. Fast completes its scan and wins p1.
    let fast = acquire(&identity, &state, "fast-contender", || config("fast"))
        .await
        .unwrap();
    let Acquired::Leader(fast) = fast else {
        panic!("fast must lead")
    };
    assert_eq!(fast.port(), ports[1]);

    // The unrelated context exits. Wait until its accept task really drops
    // the listener, then allow slow to finish its now-stale full scan.
    drop(foreign);
    loop {
        if let Ok(check) = TcpListener::bind((Ipv4Addr::LOCALHOST, ports[0])).await {
            drop(check);
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    release.notify_one();
    let slow = slow.await.unwrap();
    let mut leaders = vec![fast];
    if let Acquired::Leader(host) = slow {
        leaders.push(host);
    }

    // Not merely stale is_leader(): both retained handles own distinct live
    // listeners, authenticate the SAME flake/token, and execute wire calls.
    for host in &leaders {
        let client = ContextClient::connect(
            (Ipv4Addr::LOCALHOST, host.port()).into(),
            &token,
            "diagnostic-observer",
            identity.flake(),
        )
        .await
        .unwrap();
        let value = client.call("read", serde_json::Map::new()).await.unwrap();
        eprintln!(
            "LAB-218 live leader: port={}, flake={}, result={value}",
            host.port(),
            client.server_flake
        );
    }
    let live_ports: Vec<_> = leaders.iter().map(RunningHost::port).collect();
    sentinel_task.abort();
    assert_eq!(
        leaders.len(),
        1,
        "LAB-218: two genuinely live leaders on ports {live_ports:?}"
    );
}
