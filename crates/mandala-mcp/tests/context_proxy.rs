//! Follower-proxy spike (OpenSpec change `mandala-native-tui`, task 1.2):
//! the REAL mandala-mcp dispatch core served over a context endpoint.
//!
//! A leader instance hosts the context with `call` frames routed into
//! [`MandalaHandler::call_tool`] — the same wrapper that produces
//! seq/elapsed/result activity events — and its activity sink publishes into
//! the context's subscriber stream. A second instance detects the live
//! leader via discovery, joins as a follower, and forwards a real read tool
//! call. Gates:
//!
//! 1. the follower-forwarded result is BYTE-identical to the same call served
//!    directly at the leader;
//! 2. the call's activity events appear in the leader's stream (via a
//!    subscriber connection), origin-labeled with the follower's hello
//!    identity — while leader-local calls carry no origin.
//!
//! Origin labeling rides a tokio `task_local` scoped around the dispatch:
//! the sink runs synchronously inside the scoped call task, so it can read
//! the origin without threading it through `call_tool`'s signature. That is
//! spike-grade glue — section 2's ContextHost should carry origin explicitly
//! through the dispatch wrapper.
//!
//! The fleet is injected via the `MANDALA_AGGREGATE_FILE` seam (the stdio
//! handshake test's pattern) — no flake eval, no live fleet; state is
//! isolated via `MANDALA_FLEET_STATE`. One test fn: the seam vars are
//! process-global.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::broadcast;

use mandala_context::{Acquired, ContextClient, ContextIdentity, HostConfig, acquire, discovery};
use mandala_mcp::MandalaHandler;

tokio::task_local! {
    /// The originating client identity for the dispatch currently executing
    /// on this task (set by the context glue, read by the activity sink).
    static ORIGIN: String;
}

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
async fn follower_forwarded_call_is_byte_identical_and_origin_labeled() {
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

    // ---- the leader: real dispatch core behind the endpoint -------------
    let (events, _) = broadcast::channel::<Value>(256);
    let sink_events = events.clone();
    let handler = Arc::new(
        MandalaHandler::new(identity.flake()).with_sink(Arc::new(move |event: &Value| {
            // The dispatch wrapper's events, origin-stamped when the call
            // came over the wire (the task_local is set only inside the
            // context glue's scope).
            let mut event = event.clone();
            if let (Ok(origin), Some(obj)) =
                (ORIGIN.try_with(std::clone::Clone::clone), event.as_object_mut())
            {
                obj.insert("origin".to_string(), Value::from(origin));
            }
            let _ = sink_events.send(event);
        })),
    );
    let dispatch_handler = Arc::clone(&handler);
    let dispatch: mandala_context::Dispatch = Arc::new(move |origin, tool, args| {
        let handler = Arc::clone(&dispatch_handler);
        Box::pin(ORIGIN.scope(origin, async move {
            match handler.call_tool(&tool, args).await {
                Ok(result) => serde_json::to_value(result).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        }))
    });
    let leader = match acquire(
        &identity,
        &state_dir,
        "leader-mcp",
        HostConfig::new(dispatch, events),
    )
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

    // ---- a second instance detects the leader and forwards ---------------
    let (f_events, _) = broadcast::channel::<Value>(8);
    let follower = match acquire(
        &identity,
        &state_dir,
        "agent-claude",
        HostConfig::new(
            Arc::new(|_, _, _| Box::pin(async { Err("follower never dispatches".to_string()) })),
            f_events,
        ),
    )
    .await
    .expect("second instance acquires")
    {
        Acquired::Follower(client) => client,
        Acquired::Leader(_) => panic!("a live leader exists — must follow"),
    };
    assert_eq!(follower.server_flake, identity.flake());

    let args = json!({"selector": "@k3s"}).as_object().cloned().unwrap();
    let forwarded = follower
        .call("resolve", args.clone())
        .await
        .expect("forwarded resolve succeeds");

    // ---- gate 1: byte identity vs the leader-served call ----------------
    let direct = serde_json::to_value(
        handler
            .call_tool("resolve", args.clone())
            .await
            .expect("direct resolve succeeds"),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_string(&forwarded).unwrap(),
        serde_json::to_string(&direct).unwrap(),
        "follower-forwarded result must be byte-identical to leader-served"
    );
    // And it is the real tool result, not an echo.
    let payload: Value =
        serde_json::from_str(forwarded["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(payload, json!({"members": ["cache", "web"], "limit": "cache,web"}));

    // ---- gate 2: origin-labeled activity in the leader's stream ---------
    // Two calls happened (forwarded first, then direct): four events. The
    // forwarded pair carries the follower's hello identity; the direct pair
    // carries no origin.
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
