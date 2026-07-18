//! Golden-fixture parity OVER THE PROXY HOP (OpenSpec change
//! `mandala-native-tui`, task 3.2 — the fleet-mcp "Tool-surface parity"
//! requirement now covering the follower path).
//!
//! The same 22 fixture scenarios `tests/parity.rs` replays against the
//! directly-served handler are replayed here through a REAL context endpoint:
//! a leader hosts the scenario handler behind the coordination endpoint, and
//! a `ContextSession` follower (a second `mandala mcp` instance in miniature)
//! forwards every call. Each scenario is gated against the golden fixture
//! with the shared normalization (`tests/common/`), and every READ fixture is
//! additionally issued through the leader-local seam (`LocalContext` over the
//! same dispatch) and compared BYTE-identically with the forwarded result —
//! the proxy hop must be invisible. Mutations execute exactly once (through
//! the follower; double-execution would corrupt the registry sequence the
//! `deploy_status.list` fixture encodes) — their leader-local gate is
//! `parity.rs` itself.
//!
//! Also asserted, because only the proxy hop produces them: every mutating
//! settle in the leader's audit trail carries the follower's hello identity
//! as `origin` (fleet-context: "labeled with their originating client"), and
//! the follower-initiated `reload` swaps the LEADER's inventory so every
//! subsequent call — all executed at the leader — sees the fresh contract
//! (task 3.1's reload semantics).
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
use common::{CommandFake, EventLog, FakeEffects, check, handler, load_fixture, spacer};

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
/// the tool-error message on failure (the same surface `common::call` reads
/// off the direct handler).
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

/// A READ through BOTH seams: forwarded first, then leader-local; the two
/// full results must be byte-identical (the proxy hop is invisible). Returns
/// the structured content for the fixture check.
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
    structured(&fwd)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn golden_fixture_parity_through_the_proxy_hop() {
    // Isolated state (registry, audit, drift, context discovery) — its own
    // dir so the registry sequence matches the list fixture exactly, same as
    // parity.rs.
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
    let mut failures: Vec<String> = Vec::new();

    // ---- reads --------------------------------------------------------------
    install(&slot, handler(FakeEffects::default(), &events));
    for (fixture, tool, args) in [
        ("members.compact", "members", json!({})),
        ("members.full", "members", json!({"full": true})),
        ("groups.ok", "groups", json!({})),
        (
            "resolve.ok",
            "resolve",
            json!({"selector": "all,!@gateway"}),
        ),
    ] {
        let actual = read_both(follower, local, tool, &args).await;
        check(&mut failures, fixture, &actual);
    }

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
    let actual = read_both(follower, local, "ping", &json!({"selector": "@k3s"})).await;
    check(&mut failures, "ping.mixed", &actual);

    // ---- host_eval ----------------------------------------------------------
    install(&slot, handler(FakeEffects::default(), &events));
    let actual = read_both(follower, local, "host_eval", &json!({"member": "web"})).await;
    check(&mut failures, "host_eval.ok", &actual);

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
    let actual = read_both(
        follower,
        local,
        "host_eval",
        &json!({"member": "web", "toplevel": true}),
    )
    .await;
    check(&mut failures, "host_eval.eval_error", &actual);

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
    let actual = read_both(follower, local, "drift", &json!({})).await;
    check(&mut failures, "drift.ok", &actual);
    let actual = read_both(follower, local, "drift", &json!({"statuses": ["drift"]})).await;
    check(&mut failures, "drift.filtered", &actual);

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
    let actual = read_both(follower, local, "drift", &json!({"do_eval": true})).await;
    check(&mut failures, "drift.eval_error", &actual);

    // ---- reload (executes AT the leader; forwarded like any mutation) -------
    install(
        &slot,
        handler(
            FakeEffects {
                fresh: Some(json!({
                    "schemaVersion": 1,
                    "members": {"web": {}, "cache": {}, "router": {}, "newbie": {}},
                    "groups": {"k3s": ["cache", "web"]},
                    "projections": {"deploy": {"nodes": ["cache", "web"]}},
                })),
                ..FakeEffects::default()
            },
            &events,
        ),
    );
    let actual = structured(
        &fcall(follower, "reload", &json!({}))
            .await
            .expect("reload.ok"),
    );
    check(&mut failures, "reload.ok", &actual);
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
    let err = fcall(follower, "reload", &json!({}))
        .await
        .expect_err("reload must refuse on a non-reloadable host");
    let fixture = load_fixture("reload.unavailable_error");
    if json!({"tool_error": err}) != fixture {
        failures.push(format!(
            "reload.unavailable_error: MISMATCH\n  actual:  {err}\n  fixture: {fixture}"
        ));
    }

    // ---- actions (through the follower — executed once, at the leader) ------
    install(&slot, handler(FakeEffects::default(), &events));
    let actual = structured(
        &fcall(
            follower,
            "deploy",
            &json!({"selector": "@k3s", "dry_activate": false}),
        )
        .await
        .expect("deploy.refused"),
    );
    check(&mut failures, "deploy.refused", &actual);

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
            .expect("deploy.dry_ok"),
    );
    check(&mut failures, "deploy.dry_ok", &actual);
    spacer().await;

    install(&slot, handler(FakeEffects::default(), &events));
    let actual = structured(
        &fcall(
            follower,
            "restart_service",
            &json!({"selector": "@k3s", "unit": "k3s"}),
        )
        .await
        .expect("restart_service.refused"),
    );
    check(&mut failures, "restart_service.refused", &actual);

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
        .expect("restart_service.partial"),
    );
    check(&mut failures, "restart_service.partial", &actual);

    install(&slot, handler(FakeEffects::default(), &events));
    let actual = structured(
        &fcall(
            follower,
            "reboot",
            &json!({"selector": "@k3s", "confirm": "web"}),
        )
        .await
        .expect("reboot.refused"),
    );
    check(&mut failures, "reboot.refused", &actual);

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
        .expect("reboot.ok"),
    );
    check(&mut failures, "reboot.ok", &actual);
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
            .expect("build.ok"),
    );
    check(&mut failures, "build.ok", &actual);
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
    let actual = read_both(
        follower,
        local,
        "deploy_status",
        &json!({"run_id": cmd_run_id}),
    )
    .await;
    check(&mut failures, "deploy_status.command", &actual);
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
    let actual = read_both(
        follower,
        local,
        "deploy_status",
        &json!({"run_id": dep_run_id}),
    )
    .await;
    check(&mut failures, "deploy_status.deploy", &actual);

    let actual = read_both(follower, local, "deploy_status", &json!({"limit": 5})).await;
    check(&mut failures, "deploy_status.list", &actual);

    assert!(
        failures.is_empty(),
        "{} fixture mismatches through the proxy hop:\n\n{}",
        failures.len(),
        failures.join("\n\n")
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
    // deploy ×2 (refused + dry), restart_service ×2, reboot ×2, reload ×2
    // (ok + unavailable error) — all forwarded, all origin-labeled.
    assert_eq!(audit.len(), 8, "audit lines: {audit_text}");
    for line in &audit {
        assert_eq!(
            line["origin"], "parity-follower",
            "a forwarded mutating settle must record its originating client: {line}"
        );
    }

    leader.shutdown(Duration::from_secs(1)).await;
    unsafe { std::env::remove_var("MANDALA_FLEET_STATE") };
}
