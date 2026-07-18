//! The `FleetContext` seam over the REAL mandala-mcp dispatch core
//! (OpenSpec change `mandala-native-tui`, task 2.4 — grown from the 1.2
//! follower-proxy spike).
//!
//! One dispatch — [`MandalaHandler::call_tool_from`], the wrapper that
//! produces seq/elapsed/result activity events and the audit trail — behind
//! BOTH implementations of the seam:
//!
//! - [`LocalContext`]: leader-local execution (origin `None`);
//! - [`ContextSession`]: a second instance that detects the live leader via
//!   discovery and forwards over the wire (origin = its hello identity).
//!
//! Gates:
//!
//! 1. the same call through both impls yields BYTE-identical results — the
//!    swap point is invisible to tool code;
//! 2. origin flows EXPLICITLY through the dispatch wrapper (the 1.2
//!    `task_local` glue is retired): the forwarded call's start/settle pair
//!    is origin-labeled with the follower's hello identity in the leader's
//!    stream, while leader-local calls carry no origin key at all.
//!
//! The fleet is injected via the `MANDALA_AGGREGATE_FILE` seam (the stdio
//! handshake test's pattern) — no flake eval, no live fleet; state is
//! isolated via `MANDALA_FLEET_STATE`. One test fn: the seam vars are
//! process-global.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::broadcast;

use mandala_context::{
    Acquired, ContextClient, ContextIdentity, ContextSession, FleetContext, HostConfig,
    LocalContext, acquire, discovery,
};
use mandala_mcp::MandalaHandler;

/// Collect stream events until `pred` has matched `want` events or the
/// timeout expires.
async fn collect_events(
    stream: &mut tokio::sync::mpsc::Receiver<Value>,
    want: usize,
    pred: impl Fn(&Value) -> bool,
) -> Vec<Value> {
    let mut matched = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while matched.len() < want {
        let event = tokio::time::timeout_at(deadline, stream.recv())
            .await
            .expect("activity events within 10s")
            .expect("event stream open");
        if pred(&event) {
            matched.push(event);
        }
    }
    matched
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fleet_context_seam_is_byte_identical_and_origin_labeled() {
    // ---- fixture fleet via the aggregate seam + isolated state ----------
    let scratch = std::env::temp_dir().join(format!(
        "mandala-context-proxy-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let flake_dir = scratch.join("flake");
    let state_dir = scratch.join("state");
    std::fs::create_dir_all(&flake_dir).unwrap();
    std::fs::create_dir_all(&state_dir).unwrap();
    let aggregate = scratch.join("aggregate.json");
    std::fs::write(
        &aggregate,
        json!({
            "schemaVersion": 1,
            "members": {"web": {}, "cache": {}, "router": {}},
            "groups": {"k3s": ["cache", "web"], "gateway": ["router"]},
            "projections": {"deploy": {"nodes": ["cache", "web"]}},
        })
        .to_string(),
    )
    .unwrap();
    // One test fn in this binary: the process-global seam vars cannot race.
    unsafe {
        std::env::set_var("MANDALA_AGGREGATE_FILE", &aggregate);
        std::env::set_var("MANDALA_FLEET_STATE", &state_dir);
    }

    let identity = ContextIdentity::for_flake(&flake_dir).unwrap();

    // ---- the leader: the real dispatch core behind the endpoint ---------
    // Origin rides the dispatch signature — `call_tool_from` stamps the
    // events itself; the sink publishes them verbatim.
    let (events, _) = broadcast::channel::<Value>(256);
    let sink_events = events.clone();
    let handler = Arc::new(MandalaHandler::new(identity.flake()).with_sink(Arc::new(
        move |event: &Value| {
            let _ = sink_events.send(event.clone());
        },
    )));
    let dispatch_handler = Arc::clone(&handler);
    let dispatch: mandala_context::Dispatch =
        Arc::new(move |origin: Option<String>, tool, args| {
            let handler = Arc::clone(&dispatch_handler);
            Box::pin(async move {
                match handler.call_tool_from(origin.as_deref(), &tool, args).await {
                    Ok(result) => serde_json::to_value(result).map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                }
            })
        });
    let leader_config = HostConfig::new(dispatch.clone(), events.clone());
    let leader = match acquire(&identity, &state_dir, "leader-mcp", move || leader_config)
        .await
        .expect("first instance claims the context")
    {
        Acquired::Leader(host) => host,
        Acquired::Follower(_) => panic!("no prior context — must lead"),
    };

    // ---- an observer watches the leader's stream ------------------------
    let d = discovery::read(&state_dir, identity.key()).expect("discovery published");
    assert_eq!(d.url, leader.url());
    let observer = ContextClient::connect(
        d.addr().unwrap(),
        &d.token,
        "observer-tui",
        identity.flake(),
    )
    .await
    .expect("observer joins via discovery");
    let mut stream = observer.subscribe().await.expect("subscribed");

    // ---- the two seam implementations -----------------------------------
    // Leader-local: the injected dispatch, directly.
    let local: Box<dyn FleetContext> = Box::new(LocalContext::new(dispatch, events));
    // Follower: a second instance detects the live leader and forwards.
    let session = ContextSession::acquire(
        identity.clone(),
        &state_dir,
        "agent-claude",
        Arc::new(|| panic!("a live leader exists — the follower must never lead here")),
    )
    .await
    .expect("second instance joins as follower");
    assert!(!session.is_leader().await, "must follow the live leader");
    let follower: Box<dyn FleetContext> = Box::new(session);

    // ---- gate 1: byte identity through both impls -----------------------
    let args = json!({"selector": "@k3s"}).as_object().cloned().unwrap();
    let forwarded = follower
        .call("resolve", args.clone(), true)
        .await
        .expect("forwarded resolve succeeds");
    let direct = local
        .call("resolve", args.clone(), true)
        .await
        .expect("leader-local resolve succeeds");
    assert_eq!(
        serde_json::to_string(&forwarded).unwrap(),
        serde_json::to_string(&direct).unwrap(),
        "the seam must be invisible: byte-identical results through both impls"
    );
    // And it is the real tool result, not an echo.
    let payload: Value =
        serde_json::from_str(forwarded["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        payload,
        json!({"members": ["cache", "web"], "limit": "cache,web"})
    );

    // ---- gate 2: origin-labeled activity in the leader's stream ---------
    // Two calls happened (forwarded first, then local): four events. The
    // forwarded pair carries the follower's hello identity — threaded
    // explicitly through `call_tool_from` — the local pair no origin key.
    let resolve_events = collect_events(&mut stream, 4, |e| e["tool"] == "resolve").await;
    let (start, settle) = (&resolve_events[0], &resolve_events[1]);
    assert_eq!(start["status"], "start");
    assert_eq!(start["origin"], "agent-claude");
    assert_eq!(settle["status"], "ok");
    assert_eq!(settle["origin"], "agent-claude");
    assert_eq!(settle["seq"], start["seq"], "start/settle pair by seq");
    assert!(settle["elapsed"].is_number(), "settle carries elapsed");

    let (d_start, d_settle) = (&resolve_events[2], &resolve_events[3]);
    assert_eq!(d_start["status"], "start");
    assert_eq!(d_settle["status"], "ok");
    assert_eq!(d_start.get("origin"), None, "leader-local call: no origin");
    assert_eq!(d_settle.get("origin"), None);

    unsafe {
        std::env::remove_var("MANDALA_AGGREGATE_FILE");
        std::env::remove_var("MANDALA_FLEET_STATE");
    }
}
