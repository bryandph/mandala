//! Leader-vs-follower parity OVER THE PROXY HOP (OpenSpec change
//! `mandala-native-tui`, task 3.2; reworked in task 7.5 — the fleet-mcp
//! "Tool-surface parity" gate, now SELF-GATING: the retired Python package's
//! captured fixture oracle went with it (operator decision), so parity means
//! the proxy hop is invisible — a follower-forwarded call returns
//! byte-identical results to the same call served leader-locally).
//!
//! The full 12-tool surface runs through a REAL context endpoint: a leader
//! hosts the scenario handler behind the coordination endpoint, and a
//! `ContextSession` follower (a second `mandala mcp` instance in miniature)
//! forwards every call. Every READ is issued through BOTH seams
//! (`ContextSession` and `LocalContext` over the same dispatch) and the two
//! full serialized `CallToolResult`s must be byte-identical. Mutations cover
//! the hop two ways: their effect-free refusal/error paths run through both
//! seams (byte-identical — they touch no registry state), while the
//! effectful ok paths execute exactly ONCE, through the follower
//! (double-execution would corrupt the registry sequence the later
//! `deploy_status` reads observe).
//!
//! Also asserted, because only the proxy hop produces them: every forwarded
//! mutating settle in the leader's audit trail carries the follower's hello
//! identity as `origin` (fleet-context: "labeled with their originating
//! client") while seam-local settles carry none, and the follower-initiated
//! `reload` swaps the LEADER's inventory so every subsequent call — all
//! executed at the leader — sees the fresh contract (task 3.1's reload
//! semantics).
//!
//! One test fn: the process-global `MANDALA_FLEET_STATE` seam cannot race.

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mandala_context::{
    Acquired, CallError, ContextIdentity, ContextSession, FleetContext, HostConfig, LocalContext,
    acquire,
};
use mandala_core::registry::{self, Meta};
use mandala_core::runner::COMMAND_LOG;
use mandala_mcp::{MandalaHandler, tool_is_idempotent};
use serde_json::{Value, json};
use tokio::sync::broadcast;

mod common;
use common::{CommandFake, EventLog, FakeEffects, handler, spacer};

/// The swappable per-scenario handler the leader's one dispatch delegates to.
type Slot = Arc<RwLock<Option<Arc<MandalaHandler>>>>;

fn install(slot: &Slot, h: MandalaHandler) {
    *slot.write().unwrap() = Some(Arc::new(h));
}

/// The leader dispatch: origin-threaded into whatever scenario handler is
/// currently installed (the real `call_tool_from` wrapper every time).
fn slot_dispatch(slot: Slot) -> mandala_context::Dispatch {
    Arc::new(move |origin: Option<String>, tool: String, args| {
        let handler = slot
            .read()
            .unwrap()
            .clone()
            .expect("a scenario handler is installed");
        Box::pin(async move {
            match handler.call_tool_from(origin.as_deref(), &tool, args).await {
                Ok(result) => serde_json::to_value(result).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        })
    })
}

/// One call through a seam: the full serialized `CallToolResult` on success,
/// the tool-error message on failure.
async fn fcall(ctx: &dyn FleetContext, tool: &str, args: &Value) -> Result<Value, String> {
    let map = args.as_object().cloned().unwrap_or_default();
    match ctx.call(tool, map, tool_is_idempotent(tool)).await {
        Ok(v) => Ok(v),
        Err(CallError::Tool(msg)) => Err(msg),
        Err(other) => panic!("unexpected context-level failure for {tool}: {other:?}"),
    }
}

/// The tool's structured result out of a serialized `CallToolResult`.
fn structured(v: &Value) -> Value {
    v.get("structuredContent")
        .cloned()
        .unwrap_or_else(|| json!({}))
}

/// The result is a non-empty JSON object — the shape floor for every tool's
/// structured content (finer shape gates live in the server's unit tests and
/// `mcp_stdio.rs`).
fn assert_shaped(tool: &str, v: &Value) {
    let obj = v
        .as_object()
        .unwrap_or_else(|| panic!("{tool}: structured result must be an object: {v}"));
    assert!(
        !obj.is_empty(),
        "{tool}: structured result must be non-empty"
    );
}

/// A READ through BOTH seams: forwarded first, then leader-local; the two
/// full results must be byte-identical (the proxy hop is invisible). Returns
/// the structured content.
async fn read_both(
    follower: &dyn FleetContext,
    local: &dyn FleetContext,
    tool: &str,
    args: &Value,
) -> Value {
    assert!(tool_is_idempotent(tool), "read_both is for reads only");
    let fwd = fcall(follower, tool, args).await.expect("forwarded read");
    let loc = fcall(local, tool, args).await.expect("leader-local read");
    assert_eq!(
        serde_json::to_string(&fwd).unwrap(),
        serde_json::to_string(&loc).unwrap(),
        "{tool}: forwarded and leader-served results must be byte-identical"
    );
    let s = structured(&fwd);
    assert_shaped(tool, &s);
    s
}

/// An effect-free MUTATION path (refusal / unavailability error) through
/// BOTH seams: forwarded first, then leader-local; the outcomes must be
/// identical. Only refusal/error paths qualify — they touch no registry
/// state, so running them twice is safe (each lands its own audit line at
/// the leader, accounted for below).
async fn refuse_both(
    follower: &dyn FleetContext,
    local: &dyn FleetContext,
    tool: &str,
    args: &Value,
) -> Result<Value, String> {
    let fwd = fcall(follower, tool, args).await;
    let loc = fcall(local, tool, args).await;
    match (&fwd, &loc) {
        (Ok(f), Ok(l)) => assert_eq!(
            serde_json::to_string(f).unwrap(),
            serde_json::to_string(l).unwrap(),
            "{tool}: forwarded and leader-served refusals must be byte-identical"
        ),
        (Err(f), Err(l)) => assert_eq!(
            f, l,
            "{tool}: forwarded and leader-served errors must be identical"
        ),
        _ => panic!("{tool}: seams disagree — forwarded {fwd:?} vs leader-local {loc:?}"),
    }
    fwd
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn leader_and_follower_paths_are_byte_identical() {
    // Isolated state (registry, audit, drift, context discovery) — its own
    // dir so the registry sequence the deploy_status reads observe is exactly
    // what this test produced.
    let scratch = std::env::temp_dir().join(format!(
        "mandala-mcp-parity-proxy-{}-{:?}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let flake_dir = scratch.join("flake");
    let state = scratch.join("state");
    std::fs::create_dir_all(&flake_dir).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    unsafe { std::env::set_var("MANDALA_FLEET_STATE", &state) };

    // ---- the context: leader endpoint + follower session + local seam ------
    let identity = ContextIdentity::with_port_range(&flake_dir, 28870, 8).unwrap();
    let slot: Slot = Arc::new(RwLock::new(None));
    let (host_events, _) = broadcast::channel::<Value>(256);
    let dispatch = slot_dispatch(Arc::clone(&slot));
    let leader_config = HostConfig::new(dispatch.clone(), host_events.clone());
    let leader = match acquire(&identity, &state, "parity-leader", move || leader_config)
        .await
        .expect("first instance claims the context")
    {
        Acquired::Leader(host) => host,
        Acquired::Follower(_) => panic!("no prior context — must lead"),
    };
    let session = ContextSession::acquire(
        identity.clone(),
        &state,
        "parity-follower",
        Arc::new(|| panic!("a live leader exists — the follower must never lead here")),
    )
    .await
    .expect("second instance joins as follower");
    assert!(!session.is_leader().await, "must follow the live leader");
    let follower: &dyn FleetContext = &session;
    let local_seam = LocalContext::new(dispatch, host_events);
    let local: &dyn FleetContext = &local_seam;

    let events: EventLog = Arc::new(Mutex::new(Vec::new()));

    // ---- reads --------------------------------------------------------------
    install(&slot, handler(FakeEffects::default(), &events));
    let members = read_both(follower, local, "members", &json!({})).await;
    assert_eq!(
        members.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["cache", "router", "web"]
    );
    read_both(follower, local, "members", &json!({"full": true})).await;
    let groups = read_both(follower, local, "groups", &json!({})).await;
    assert!(groups.get("k3s").is_some(), "groups: {groups}");
    let resolved = read_both(
        follower,
        local,
        "resolve",
        &json!({"selector": "all,!@gateway"}),
    )
    .await;
    assert_eq!(resolved["members"], json!(["cache", "web"]));

    // ---- ping ---------------------------------------------------------------
    install(
        &slot,
        handler(
            FakeEffects {
                adhoc: Some(Ok(mandala_mcp::effects::AdhocOutput {
                    stdout: "web | SUCCESS => {\"ping\": \"pong\"}\ncache | UNREACHABLE! => {}\n"
                        .to_string(),
                    stderr: "[ERROR]: remote: Counting objects: 100% (14/14)\n".to_string(),
                    code: 4,
                })),
                ..FakeEffects::default()
            },
            &events,
        ),
    );
    read_both(follower, local, "ping", &json!({"selector": "@k3s"})).await;

    // ---- host_eval ----------------------------------------------------------
    install(&slot, handler(FakeEffects::default(), &events));
    read_both(follower, local, "host_eval", &json!({"member": "web"})).await;

    install(
        &slot,
        handler(
            FakeEffects {
                eval: Some(Err(mandala_mcp::effects::EvalFailure {
                    command: Some(
                        ["nix", "eval", "--json", ".#nixosConfigurations.web..."]
                            .map(String::from)
                            .to_vec(),
                    ),
                    exit_code: Some(1),
                    output: "error: attribute 'web' missing".to_string(),
                })),
                ..FakeEffects::default()
            },
            &events,
        ),
    );
    read_both(
        follower,
        local,
        "host_eval",
        &json!({"member": "web", "toplevel": true}),
    )
    .await;

    // ---- drift --------------------------------------------------------------
    install(
        &slot,
        handler(
            FakeEffects {
                rev: Some("deadbeef".to_string()),
                ..FakeEffects::default()
            },
            &events,
        ),
    );
    read_both(follower, local, "drift", &json!({})).await;
    read_both(follower, local, "drift", &json!({"statuses": ["drift"]})).await;

    install(
        &slot,
        handler(
            FakeEffects {
                rev: Some("deadbeef".to_string()),
                refresh: Some(mandala_mcp::effects::SurveyOutput {
                    stdout: "PLAY RECAP\nweb : failed=1\n".to_string(),
                    stderr: "survey transport failed".to_string(),
                    code: 2,
                }),
                ..FakeEffects::default()
            },
            &events,
        ),
    );
    let failed_refresh = read_both(follower, local, "drift", &json!({"refresh": true})).await;
    assert_eq!(failed_refresh["ok"], json!(false));
    assert_eq!(failed_refresh["refreshed"], json!(false));
    assert_eq!(failed_refresh["survey_rc"], json!(2));
    assert_eq!(
        failed_refresh["refresh_error"]["stderr"],
        json!("survey transport failed")
    );

    install(
        &slot,
        handler(
            FakeEffects {
                rev: Some("deadbeef".to_string()),
                eval: Some(Err(mandala_mcp::effects::EvalFailure {
                    command: Some(["nix", "eval", "--json"].map(String::from).to_vec()),
                    exit_code: Some(1),
                    output: "error: eval failed".to_string(),
                })),
                ..FakeEffects::default()
            },
            &events,
        ),
    );
    read_both(follower, local, "drift", &json!({"do_eval": true})).await;

    // ---- reload (executes AT the leader; forwarded like any mutation) -------
    install(
        &slot,
        handler(
            FakeEffects {
                fresh: Some(json!({
                    "schemaVersion": 1,
                    "members": {
                        "web": {"name": "web"},
                        "cache": {"name": "cache"},
                        "router": {"name": "router"},
                        "newbie": {"name": "newbie"},
                    },
                    "groups": {"k3s": ["cache", "web"]},
                    "projections": {"deploy": {"nodes": ["cache", "web"]}},
                })),
                ..FakeEffects::default()
            },
            &events,
        ),
    );
    let reloaded = structured(
        &fcall(follower, "reload", &json!({}))
            .await
            .expect("reload ok"),
    );
    assert_shaped("reload", &reloaded);
    // The LEADER's inventory swapped: every instance's subsequent calls see
    // the fresh contract — the follower's next read AND the leader-local seam
    // (byte-identically), since they all execute at the leader.
    let after = read_both(follower, local, "resolve", &json!({"selector": "all"})).await;
    assert_eq!(
        after["members"],
        json!(["cache", "newbie", "router", "web"]),
        "a follower-initiated reload must swap the inventory every instance sees"
    );

    install(
        &slot,
        handler(FakeEffects::default(), &events).reloadable(false),
    );
    let err = refuse_both(follower, local, "reload", &json!({}))
        .await
        .expect_err("reload must refuse on a non-reloadable host");
    assert!(
        !err.is_empty(),
        "reload unavailability must carry a message"
    );

    // ---- actions: refusal paths through BOTH seams (effect-free) ------------
    install(&slot, handler(FakeEffects::default(), &events));
    let refused = structured(
        &refuse_both(
            follower,
            local,
            "deploy",
            &json!({"selector": "@k3s", "dry_activate": false}),
        )
        .await
        .expect("deploy refusal is a structured result"),
    );
    assert_shaped("deploy", &refused);

    let refused = structured(
        &refuse_both(
            follower,
            local,
            "restart_service",
            &json!({"selector": "@k3s", "unit": "k3s"}),
        )
        .await
        .expect("restart_service refusal is a structured result"),
    );
    assert_shaped("restart_service", &refused);

    let refused = structured(
        &refuse_both(
            follower,
            local,
            "reboot",
            &json!({"selector": "@k3s", "confirm": "web"}),
        )
        .await
        .expect("reboot refusal is a structured result"),
    );
    assert_shaped("reboot", &refused);

    // ---- actions: ok paths through the follower — executed exactly once -----
    install(
        &slot,
        handler(
            FakeEffects {
                fake_deploy: true,
                ..FakeEffects::default()
            },
            &events,
        ),
    );
    let actual = structured(
        &fcall(follower, "deploy", &json!({"selector": "@k3s"}))
            .await
            .expect("dry deploy ok"),
    );
    assert_shaped("deploy", &actual);
    spacer().await;

    install(
        &slot,
        handler(
            FakeEffects {
                adhoc: Some(Ok(mandala_mcp::effects::AdhocOutput {
                    stdout: "cache | CHANGED => {}\nweb | FAILED! => {}\n".to_string(),
                    stderr: String::new(),
                    code: 2,
                })),
                ..FakeEffects::default()
            },
            &events,
        ),
    );
    let actual = structured(
        &fcall(
            follower,
            "restart_service",
            &json!({"selector": "@k3s", "unit": "k3s", "confirm": "cache,web"}),
        )
        .await
        .expect("restart_service partial ok"),
    );
    assert_shaped("restart_service", &actual);

    install(
        &slot,
        handler(
            FakeEffects {
                reboot_available: true,
                fake_command: Some(CommandFake::Reboot),
                ..FakeEffects::default()
            },
            &events,
        ),
    );
    let actual = structured(
        &fcall(
            follower,
            "reboot",
            &json!({"selector": "@k3s", "serial": "2", "confirm": "cache,web"}),
        )
        .await
        .expect("reboot ok"),
    );
    assert_shaped("reboot", &actual);
    spacer().await;

    install(
        &slot,
        handler(
            FakeEffects {
                fake_command: Some(CommandFake::Build),
                ..FakeEffects::default()
            },
            &events,
        ),
    );
    let actual = structured(
        &fcall(follower, "build", &json!({"selector": "@k3s"}))
            .await
            .expect("build ok"),
    );
    assert_shaped("build", &actual);
    spacer().await;

    // ---- deploy_status ------------------------------------------------------
    let (cmd_run_id, cmd_path) = registry::new_run_dir().unwrap();
    let mut meta = Meta::new();
    meta.insert("run_id".to_string(), Value::from(cmd_run_id.clone()));
    meta.insert("kind".to_string(), Value::from("reboot"));
    meta.insert("pid".to_string(), Value::Null);
    meta.insert("rc".to_string(), Value::from(2));
    meta.insert("limit".to_string(), Value::from("web"));
    registry::write_meta(&cmd_path, &meta).unwrap();
    std::fs::write(
        cmd_path.join(COMMAND_LOG),
        "$ ans-reboot -l web\nfatal: boom\n",
    )
    .unwrap();

    install(&slot, handler(FakeEffects::default(), &events));
    read_both(
        follower,
        local,
        "deploy_status",
        &json!({"run_id": cmd_run_id}),
    )
    .await;
    spacer().await;

    let (dep_run_id, dep_path) = registry::new_run_dir().unwrap();
    let mut meta = Meta::new();
    meta.insert("run_id".to_string(), Value::from(dep_run_id.clone()));
    meta.insert("pid".to_string(), Value::Null);
    meta.insert("limit".to_string(), Value::from("cache"));
    registry::write_meta(&dep_path, &meta).unwrap();
    {
        use std::io::Write;
        let mut fh = std::fs::File::create(dep_path.join("cache.jsonl")).unwrap();
        for m in ["eval", "build", "copy", "activate", "confirm"] {
            writeln!(
                fh,
                "{}",
                json!({
                    "v": 1, "host": "cache", "plugin": "deploy",
                    "event": "milestone", "milestone": m,
                })
            )
            .unwrap();
        }
    }
    read_both(
        follower,
        local,
        "deploy_status",
        &json!({"run_id": dep_run_id}),
    )
    .await;

    let listing = read_both(follower, local, "deploy_status", &json!({"limit": 5})).await;
    assert!(
        listing["runs"].as_array().is_some_and(|r| !r.is_empty()),
        "the accumulated registry sequence must list: {listing}"
    );

    // ---- origin labeling ----------------------------------------------------
    {
        let events = events.lock().unwrap();
        assert!(
            events
                .iter()
                .filter(|e| e.get("origin").is_some())
                .all(|e| e["origin"] == "parity-follower"),
            "every origin-labeled event carries the follower's hello identity"
        );
        assert!(
            events.iter().any(|e| e.get("origin").is_none()),
            "leader-local (seam) calls carry no origin key"
        );
        let reboot_settle = events
            .iter()
            .find(|e| e["tool"] == "reboot" && e["status"] == "ok" && e["result"]["ok"] == true)
            .expect("reboot ok settle");
        assert_eq!(reboot_settle["origin"], "parity-follower");
    }

    // ---- audit: mutating settles land at the leader, origin-labeled ---------
    let audit_path = state.join("mcp").join("audit.jsonl");
    let audit_text = std::fs::read_to_string(&audit_path).expect("audit.jsonl exists");
    let audit: Vec<Value> = audit_text
        .lines()
        .map(|l| serde_json::from_str(l).expect("audit line is JSON"))
        .collect();
    // Forwarded (origin-labeled): deploy ×2 (refused + dry), restart_service
    // ×2 (refused + partial), reboot ×2 (refused + ok), reload ×2 (ok +
    // unavailable). Leader-local (no origin): the 4 seam-side refusal/error
    // replays (deploy, restart_service, reboot, reload-unavailable).
    let (forwarded, seam): (Vec<_>, Vec<_>) = audit.iter().partition(|l| l.get("origin").is_some());
    assert_eq!(forwarded.len(), 8, "audit lines: {audit_text}");
    assert_eq!(seam.len(), 4, "audit lines: {audit_text}");
    for line in &forwarded {
        assert_eq!(
            line["origin"], "parity-follower",
            "a forwarded mutating settle must record its originating client: {line}"
        );
    }

    leader.shutdown(Duration::from_secs(1)).await;
    unsafe { std::env::remove_var("MANDALA_FLEET_STATE") };
}
