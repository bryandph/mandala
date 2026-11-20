//! LAB-218 serialized disappearing-occupant regression and abrupt stress.
//! The deliberately failing original is retained in diagnostic commit 130c29b.
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

mod common;

use common::acquire;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use mandala_context::{
    Acquired, ContextClient, ContextIdentity, Discovery, FleetContext, HostConfig, RunningHost,
    discovery,
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
        background_tasks: Arc::default(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
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
    common::refusal_preflight();
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
        // Calls are blocked through abrupt host loss; tracked teardown must
        // actually terminate them before the replacement can lead.
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
                common::session_acquire(
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
        assert!(
            results.iter().all(|result| result == &results[0]),
            "trial={trial}: common dispatch results"
        );
        let d = discovery::read(&state, identity.key()).unwrap();
        let mut endpoints = 0;
        for port in identity.ports() {
            if let Ok(client) = ContextClient::connect(
                (Ipv4Addr::LOCALHOST, port).into(),
                &d.token,
                "stress-observer",
                identity.flake(),
            )
            .await
            {
                endpoints += 1;
                assert_eq!(
                    client.call("read", serde_json::Map::new()).await.unwrap(),
                    results[0],
                    "trial={trial}: wire/local dispatch agree"
                );
            }
        }
        assert_eq!(
            endpoints, 1,
            "trial={trial}: exactly one authenticated serving endpoint"
        );
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
    common::refusal_preflight();
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
    let options = common::options(&state);
    let identity = ContextIdentity::with_port_range(&flake, 29800, 3).unwrap();
    let ports: Vec<_> = identity.ports().collect();
    let foreign = RunningHost::start(
        TcpListener::bind((Ipv4Addr::LOCALHOST, ports[0]))
            .await
            .unwrap(),
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
                let _ = w.write_all(b"{\"type\":\"error\",\"error\":\"unauthorized\",\"flake\":\"/foreign/sentinel\"}\n").await;
            });
        }
    });
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
    let slow_options = options.clone();
    let slow = tokio::spawn(async move {
        mandala_context::acquire_with_options(
            &slow_id,
            &slow_state,
            "slow-contender",
            slow_options,
            || config("slow"),
        )
        .await
        .unwrap()
    });
    scanned.notified().await;
    assert!(
        mandala_context::election::try_acquire(&identity, &options)
            .unwrap()
            .is_none(),
        "slow owns both guards while gated"
    );
    let attempted = Arc::new(Notify::new());
    let mut fast_options = options.clone();
    fast_options.election_attempt = Some(attempted.clone());
    let fast_id = identity.clone();
    let fast_state = state.clone();
    let fast = tokio::spawn(async move {
        mandala_context::acquire_with_options(
            &fast_id,
            &fast_state,
            "fast-contender",
            fast_options,
            || panic!("loser factory must not run"),
        )
        .await
        .unwrap()
    });
    attempted.notified().await;
    assert!(
        !fast.is_finished(),
        "fast actually attempted acquisition and remains a candidate"
    );
    foreign.shutdown(Duration::ZERO).await;
    release.notify_one();
    let Acquired::Leader(slow) = slow.await.unwrap() else {
        panic!("slow must lead");
    };
    let Acquired::Follower(fast) = fast.await.unwrap() else {
        panic!("fast must join slow");
    };
    assert_eq!(
        fast.call("read", serde_json::Map::new()).await.unwrap()["served_by"],
        "slow"
    );
    let mut endpoints = 0;
    for port in identity.ports() {
        if let Ok(client) = ContextClient::connect(
            (Ipv4Addr::LOCALHOST, port).into(),
            &token,
            "observer",
            identity.flake(),
        )
        .await
        {
            endpoints += 1;
            assert_eq!(
                client.call("read", serde_json::Map::new()).await.unwrap()["served_by"],
                "slow"
            );
        }
    }
    assert_eq!(
        endpoints, 1,
        "exactly one authenticated wire-serving endpoint"
    );
    slow.shutdown(Duration::ZERO).await;
    sentinel_task.abort();
    let _ = sentinel_task.await;
    std::fs::remove_dir_all(root).unwrap();
}
