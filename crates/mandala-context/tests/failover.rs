//! Promotion + retry-discipline gates (OpenSpec change `mandala-native-tui`,
//! task 2.3): leader death mid-call surfaces a structured failover; the
//! session re-races the bind (winner promotes via the factory hook, losers
//! reconnect); idempotent reads retry exactly once, mutations never; the
//! subscription stream resumes across a promotion.
//!
//! Leader death is simulated by DROPPING a `RunningHost` (abrupt: no drain,
//! no discovery release — connections die, the stale file stays, exactly
//! like a killed process).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::broadcast;

use mandala_context::{
    Acquired, CallError, ContextIdentity, ContextSession, FleetContext, HostConfig,
    HostConfigFactory, acquire, discovery,
};

/// A per-test scratch tree: `flake/` (the canonicalizable checkout stand-in)
/// and `state/` (the isolated mandala state dir).
fn scratch(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!(
        "mandala-context-failover-{tag}-{}-{}",
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

/// A dispatch whose results say who served them (`served_by`), counting its
/// invocations, honoring `sleep_ms`, and publishing one settle event.
fn labeled_dispatch(
    label: &str,
    calls: Arc<AtomicUsize>,
    events: broadcast::Sender<Value>,
) -> mandala_context::Dispatch {
    let label = label.to_string();
    Arc::new(move |_origin, tool, args| {
        let label = label.clone();
        let calls = Arc::clone(&calls);
        let events = events.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            if let Some(ms) = args.get("sleep_ms").and_then(Value::as_u64) {
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
            let _ = events.send(json!({"tool": tool, "served_by": label}));
            Ok(json!({"tool": tool, "served_by": label, "args": args}))
        })
    })
}

/// A leader-side config factory for `label`, its call counter, and its
/// events sender (held by the test to publish/count).
fn factory(
    label: &str,
    calls: Arc<AtomicUsize>,
    events: broadcast::Sender<Value>,
) -> HostConfigFactory {
    let label = label.to_string();
    Arc::new(move || HostConfig {
        dispatch: labeled_dispatch(&label, Arc::clone(&calls), events.clone()),
        events: events.clone(),
        heartbeat_interval: HostConfig::DEFAULT_HEARTBEAT,
    })
}

/// Spin up leader "A" over `identity` and return its host handle.
async fn lead_as_a(
    identity: &ContextIdentity,
    state: &std::path::Path,
) -> mandala_context::RunningHost {
    let (events, _) = broadcast::channel(64);
    let calls = Arc::new(AtomicUsize::new(0));
    let cfg = factory("A", calls, events);
    match acquire(identity, state, "leader-a", move || (cfg)())
        .await
        .unwrap()
    {
        Acquired::Leader(host) => host,
        Acquired::Follower(_) => panic!("no prior context — A must lead"),
    }
}

// ---- kill a leader mid-call -------------------------------------------------

/// An idempotent read whose leader dies mid-call is retried exactly once
/// after the re-race: the session promotes (the factory hook supplies the
/// fresh dispatch) and the retry succeeds locally.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idempotent_read_retries_once_after_promotion() {
    let (flake, state) = scratch("retry");
    let identity = ContextIdentity::with_port_range(&flake, 28800, 4).unwrap();
    let leader_a = lead_as_a(&identity, &state).await;

    let (events_s, _) = broadcast::channel(64);
    let calls_s = Arc::new(AtomicUsize::new(0));
    let session = ContextSession::acquire(
        identity.clone(),
        &state,
        "session-s",
        factory("S", Arc::clone(&calls_s), events_s),
    )
    .await
    .unwrap();
    assert!(!session.is_leader().await, "A leads; S must follow");

    let call_session = session.clone();
    let in_flight = tokio::spawn(async move {
        call_session
            .call(
                "read",
                json!({"sleep_ms": 500}).as_object().cloned().unwrap(),
                true,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(leader_a); // the leader dies under the call

    let result = in_flight
        .await
        .unwrap()
        .expect("an idempotent read must survive leader death via one retry");
    assert_eq!(
        result["served_by"],
        json!("S"),
        "the retry ran on the promoted session's own dispatch"
    );
    assert_eq!(
        calls_s.load(Ordering::SeqCst),
        1,
        "exactly one retry — never more"
    );
    assert!(session.is_leader().await, "the session promoted");

    // Promotion republished discovery around the new leader.
    let d = discovery::read(&state, identity.key()).expect("discovery rewritten");
    assert_eq!(d.pid, std::process::id());
    assert!(identity.ports().any(|p| d.url.ends_with(&p.to_string())));
}

/// A mutation whose leader dies mid-call is NEVER retried: the session still
/// re-races (and here promotes), but the caller gets the structured
/// failover error and the fresh dispatch is never invoked for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mutation_surfaces_structured_failover_without_retry() {
    let (flake, state) = scratch("mutation");
    let identity = ContextIdentity::with_port_range(&flake, 28805, 4).unwrap();
    let leader_a = lead_as_a(&identity, &state).await;

    let (events_s, _) = broadcast::channel(64);
    let calls_s = Arc::new(AtomicUsize::new(0));
    let session = ContextSession::acquire(
        identity.clone(),
        &state,
        "session-s",
        factory("S", Arc::clone(&calls_s), events_s),
    )
    .await
    .unwrap();

    let call_session = session.clone();
    let in_flight = tokio::spawn(async move {
        call_session
            .call(
                "deploy",
                json!({"sleep_ms": 500}).as_object().cloned().unwrap(),
                false,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(leader_a);

    match in_flight.await.unwrap() {
        Err(CallError::Failover { retried, promoted }) => {
            assert!(!retried, "mutations must never be retried");
            assert!(promoted, "no other candidate — the session promotes");
        }
        other => panic!("expected the structured failover error, got {other:?}"),
    }
    assert_eq!(
        calls_s.load(Ordering::SeqCst),
        0,
        "the mutation must not have re-executed anywhere"
    );
    assert!(session.is_leader().await);
}

// ---- simultaneous promotion -------------------------------------------------

/// Three sessions lose their leader at once and all re-race through their
/// calls: exactly one promotes, the others reconnect to it, and every
/// retried read succeeds — served by the one winner.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_promotion_race_has_exactly_one_winner() {
    let (flake, state) = scratch("race");
    let identity = ContextIdentity::with_port_range(&flake, 28810, 4).unwrap();
    let leader_a = lead_as_a(&identity, &state).await;

    let mut sessions = Vec::new();
    for i in 0..3 {
        let (events, _) = broadcast::channel(64);
        let calls = Arc::new(AtomicUsize::new(0));
        let session = ContextSession::acquire(
            identity.clone(),
            &state,
            &format!("session-{i}"),
            factory(&format!("s{i}"), calls, events),
        )
        .await
        .unwrap();
        assert!(!session.is_leader().await);
        sessions.push(session);
    }

    let mut in_flight = Vec::new();
    for session in &sessions {
        let session = session.clone();
        in_flight.push(tokio::spawn(async move {
            session
                .call(
                    "read",
                    json!({"sleep_ms": 500}).as_object().cloned().unwrap(),
                    true,
                )
                .await
        }));
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(leader_a);

    let mut served_by = Vec::new();
    for task in in_flight {
        let result = task
            .await
            .unwrap()
            .expect("every session's retried read succeeds");
        served_by.push(result["served_by"].as_str().unwrap().to_string());
    }

    let mut leaders = 0;
    let mut winner = None;
    for (i, session) in sessions.iter().enumerate() {
        if session.is_leader().await {
            leaders += 1;
            winner = Some(format!("s{i}"));
        }
    }
    assert_eq!(leaders, 1, "the bind arbitrates exactly one promotion");
    let winner = winner.unwrap();
    for label in &served_by {
        assert_eq!(label, &winner, "every retry was served by the one winner");
    }
}

// ---- subscription resumption ------------------------------------------------

/// A session's subscription stream survives its leader: events flow from A,
/// A dies, the session promotes, and events keep flowing from its own
/// context — one receiver, no re-subscribe by the caller.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscribe_stream_resumes_across_promotion() {
    let (flake, state) = scratch("resume");
    let identity = ContextIdentity::with_port_range(&flake, 28815, 4).unwrap();

    let (events_a, _) = broadcast::channel(64);
    let calls_a = Arc::new(AtomicUsize::new(0));
    let cfg_a = factory("A", calls_a, events_a.clone());
    let leader_a = match acquire(&identity, &state, "leader-a", move || (cfg_a)())
        .await
        .unwrap()
    {
        Acquired::Leader(host) => host,
        Acquired::Follower(_) => panic!("A must lead"),
    };

    let (events_s, _) = broadcast::channel(64);
    let calls_s = Arc::new(AtomicUsize::new(0));
    let session = ContextSession::acquire(
        identity.clone(),
        &state,
        "session-s",
        factory("S", calls_s, events_s.clone()),
    )
    .await
    .unwrap();
    let mut stream = session.subscribe().await.unwrap();

    // Follower phase: an event published at A reaches the stream.
    let _ = events_a.send(json!({"marker": "from-a"}));
    let first = tokio::time::timeout(Duration::from_secs(5), stream.recv())
        .await
        .expect("event from A within 5s")
        .expect("stream open");
    assert_eq!(first["marker"], json!("from-a"));

    drop(leader_a); // the leader dies

    // The session re-races and resumes from its OWN context. Publish the
    // post-promotion marker repeatedly (sends before the local subscription
    // attaches are lost by design) until it comes through.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let resumed = loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "stream must resume after promotion"
        );
        let _ = events_s.send(json!({"marker": "from-s"}));
        match tokio::time::timeout(Duration::from_millis(50), stream.recv()).await {
            Ok(Some(event)) if event["marker"] == json!("from-s") => break event,
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => panic!("stream closed instead of resuming"),
        }
    };
    assert_eq!(resumed["marker"], json!("from-s"));
    assert!(session.is_leader().await, "the session promoted");
}
