//! Golden-fixture parity: replay the Python FastMCP server's recorded result
//! shapes (`cli/tests/fixtures/mcp/*.json` — the parity oracle for OpenSpec
//! change `mandala-rust-rewrite`, section 4) against the Rust server.
//!
//! Every fixture is driven through the REAL dispatch path
//! (`MandalaHandler::call_tool`, activity wrapper included) over the same
//! injected aggregate `capture_fixtures.py` used, with the subprocess/launch
//! seams faked exactly the way the Python capture monkeypatched them
//! (`subprocess.run`, `drift.eval_expected`, `drift.repo_rev`,
//! `DeployRun.start`, `CommandRun`, `shutil.which`+`Popen`). Sandbox-safe: no
//! ansible, nix, or network.
//!
//! Volatile fields (`run_id`, `events_dir`, `log`, `pid`, `elapsed`, `ts`,
//! `started_at`/`finished_at`) are normalized to the same placeholders the
//! capture script wrote before comparing — parity is keys + non-volatile
//! values, per the fixtures' README.
//!
//! Everything runs in ONE sequential test: the fixture scenarios share a
//! process-wide `MANDALA_FLEET_STATE` and the `deploy_status.list` fixture
//! depends on the exact accumulated registry state, in capture order.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use mandala_core::registry::{self, Meta};
use mandala_core::runner::COMMAND_LOG;
use mandala_mcp::effects::{AdhocOutput, EvalFailure};
use serde_json::{Value, json};

// The fixture loader, normalization, injected fleet, and fake effects live in
// `tests/common/` — shared verbatim with the proxy-hop re-gate
// (`tests/parity_proxy.rs`, mandala-native-tui task 3.2).
mod common;
use common::{CommandFake, EventLog, FakeEffects, call, check, handler, load_fixture, spacer};

#[tokio::test(flavor = "current_thread")]
async fn golden_fixture_parity() {
    // Isolate ALL state (run registry, audit.jsonl, drift snapshots) into a
    // throwaway dir — the same `MANDALA_FLEET_STATE` isolation the capture
    // script used. This binary holds exactly one test, so the process env is
    // ours to set before anything reads it.
    let state = std::env::temp_dir().join(format!(
        "mandala-mcp-parity-{}-{:?}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&state).unwrap();
    unsafe { std::env::set_var("MANDALA_FLEET_STATE", &state) };

    let events: EventLog = Arc::new(Mutex::new(Vec::new()));
    let mut failures: Vec<String> = Vec::new();

    // ---- reads (capture_reads) ---------------------------------------------
    let h = handler(FakeEffects::default(), &events);
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
        let actual = call(&h, tool, args).await.expect(fixture);
        check(&mut failures, fixture, &actual);
    }

    // ---- ping (capture_ping) -----------------------------------------------
    let h = handler(
        FakeEffects {
            adhoc: Some(Ok(AdhocOutput {
                stdout: "web | SUCCESS => {\"ping\": \"pong\"}\ncache | UNREACHABLE! => {}\n"
                    .to_string(),
                stderr: "[ERROR]: remote: Counting objects: 100% (14/14)\n".to_string(),
                code: 4,
            })),
            ..FakeEffects::default()
        },
        &events,
    );
    let actual = call(&h, "ping", json!({"selector": "@k3s"}))
        .await
        .expect("ping.mixed");
    check(&mut failures, "ping.mixed", &actual);

    // ---- host_eval (capture_host_eval) -------------------------------------
    let h = handler(FakeEffects::default(), &events);
    let actual = call(&h, "host_eval", json!({"member": "web"}))
        .await
        .expect("host_eval.ok");
    check(&mut failures, "host_eval.ok", &actual);

    let h = handler(
        FakeEffects {
            eval: Some(Err(EvalFailure {
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
    );
    let actual = call(&h, "host_eval", json!({"member": "web", "toplevel": true}))
        .await
        .expect("host_eval.eval_error");
    check(&mut failures, "host_eval.eval_error", &actual);

    // ---- drift (capture_drift) ---------------------------------------------
    let h = handler(
        FakeEffects {
            rev: Some("deadbeef".to_string()),
            ..FakeEffects::default()
        },
        &events,
    );
    // No snapshots, no eval → expected_source none, both nodes no-snapshot.
    let actual = call(&h, "drift", json!({})).await.expect("drift.ok");
    check(&mut failures, "drift.ok", &actual);
    // Status filter narrows entries; summary stays whole-fleet.
    let actual = call(&h, "drift", json!({"statuses": ["drift"]}))
        .await
        .expect("drift.filtered");
    check(&mut failures, "drift.filtered", &actual);

    let h = handler(
        FakeEffects {
            rev: Some("deadbeef".to_string()),
            eval: Some(Err(EvalFailure {
                command: Some(["nix", "eval", "--json"].map(String::from).to_vec()),
                exit_code: Some(1),
                output: "error: eval failed".to_string(),
            })),
            ..FakeEffects::default()
        },
        &events,
    );
    let actual = call(&h, "drift", json!({"do_eval": true}))
        .await
        .expect("drift.eval_error");
    check(&mut failures, "drift.eval_error", &actual);

    // ---- reload (capture_reload) -------------------------------------------
    let h = handler(
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
    );
    let actual = call(&h, "reload", json!({})).await.expect("reload.ok");
    check(&mut failures, "reload.ok", &actual);
    // The swapped inventory is what subsequent tools serve.
    let after = call(&h, "resolve", json!({"selector": "all"}))
        .await
        .expect("resolve after reload");
    assert_eq!(
        after["members"],
        json!(["cache", "newbie", "router", "web"]),
        "reload must swap the served inventory"
    );

    // Unavailable path: a host that cannot swap the inventory.
    let h = handler(FakeEffects::default(), &events).reloadable(false);
    let err = call(&h, "reload", json!({}))
        .await
        .expect_err("reload must refuse on a non-reloadable host");
    let fixture = load_fixture("reload.unavailable_error");
    if json!({"tool_error": err}) != fixture {
        failures.push(format!(
            "reload.unavailable_error: MISMATCH\n  actual:  {err}\n  fixture: {fixture}"
        ));
    }

    // ---- actions (capture_actions) -----------------------------------------
    // deploy: refusal (real activation without confirm) — creates NO run.
    let h = handler(FakeEffects::default(), &events);
    let actual = call(
        &h,
        "deploy",
        json!({"selector": "@k3s", "dry_activate": false}),
    )
    .await
    .expect("deploy.refused");
    check(&mut failures, "deploy.refused", &actual);

    // deploy: dry-run launch (stubbed start — registry dir, no subprocess).
    let h = handler(
        FakeEffects {
            fake_deploy: true,
            ..FakeEffects::default()
        },
        &events,
    );
    let actual = call(&h, "deploy", json!({"selector": "@k3s"}))
        .await
        .expect("deploy.dry_ok");
    check(&mut failures, "deploy.dry_ok", &actual);
    spacer().await;

    // restart_service: refusal.
    let h = handler(FakeEffects::default(), &events);
    let actual = call(
        &h,
        "restart_service",
        json!({"selector": "@k3s", "unit": "k3s"}),
    )
    .await
    .expect("restart_service.refused");
    check(&mut failures, "restart_service.refused", &actual);

    // restart_service: partial (monkeypatched ansible, rc 2).
    let h = handler(
        FakeEffects {
            adhoc: Some(Ok(AdhocOutput {
                stdout: "cache | CHANGED => {}\nweb | FAILED! => {}\n".to_string(),
                stderr: String::new(),
                code: 2,
            })),
            ..FakeEffects::default()
        },
        &events,
    );
    let actual = call(
        &h,
        "restart_service",
        json!({"selector": "@k3s", "unit": "k3s", "confirm": "cache,web"}),
    )
    .await
    .expect("restart_service.partial");
    check(&mut failures, "restart_service.partial", &actual);

    // reboot: refusal (mismatched confirm).
    let h = handler(FakeEffects::default(), &events);
    let actual = call(&h, "reboot", json!({"selector": "@k3s", "confirm": "web"}))
        .await
        .expect("reboot.refused");
    check(&mut failures, "reboot.refused", &actual);

    // reboot: ok launch (fake ans-reboot + fake Popen).
    let h = handler(
        FakeEffects {
            reboot_available: true,
            fake_command: Some(CommandFake::Reboot),
            ..FakeEffects::default()
        },
        &events,
    );
    let actual = call(
        &h,
        "reboot",
        json!({"selector": "@k3s", "serial": "2", "confirm": "cache,web"}),
    )
    .await
    .expect("reboot.ok");
    check(&mut failures, "reboot.ok", &actual);
    let reboot_run_id = actual["run_id"].as_str().unwrap_or_default().to_string();
    spacer().await;

    // build: ok (fake CommandRun writing a teed log with out-paths).
    let h = handler(
        FakeEffects {
            fake_command: Some(CommandFake::Build),
            ..FakeEffects::default()
        },
        &events,
    );
    let actual = call(&h, "build", json!({"selector": "@k3s"}))
        .await
        .expect("build.ok");
    check(&mut failures, "build.ok", &actual);
    spacer().await;

    // ---- deploy_status (capture_deploy_status) -----------------------------
    // A reboot command run that failed.
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

    let h = handler(FakeEffects::default(), &events);
    let actual = call(&h, "deploy_status", json!({"run_id": cmd_run_id}))
        .await
        .expect("deploy_status.command");
    check(&mut failures, "deploy_status.command", &actual);
    spacer().await;

    // A deploy run with a host that reached confirmed.
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
    let actual = call(&h, "deploy_status", json!({"run_id": dep_run_id}))
        .await
        .expect("deploy_status.deploy");
    check(&mut failures, "deploy_status.deploy", &actual);

    // The list form (most-recent runs): exactly the five runs the scenario
    // sequence registered, newest first.
    let actual = call(&h, "deploy_status", json!({"limit": 5}))
        .await
        .expect("deploy_status.list");
    check(&mut failures, "deploy_status.list", &actual);

    assert!(
        failures.is_empty(),
        "{} fixture mismatches:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );

    // ---- activity events (the dispatch wrapper) ----------------------------
    let events = events.lock().unwrap();
    // Every call emitted a start + a settle sharing a seq.
    let starts: Vec<&Value> = events.iter().filter(|e| e["status"] == "start").collect();
    let settles: Vec<&Value> = events
        .iter()
        .filter(|e| e["status"] == "ok" || e["status"] == "error")
        .collect();
    assert_eq!(starts.len(), settles.len(), "unpaired activity events");
    for settle in &settles {
        let seq = &settle["seq"];
        assert!(
            starts
                .iter()
                .any(|s| &s["seq"] == seq && s["tool"] == settle["tool"]),
            "settle without a matching start: {settle}"
        );
        assert!(
            settle["elapsed"].is_number(),
            "settle lacks elapsed: {settle}"
        );
    }
    // The reboot ok-settle carries the exact run to attach.
    let reboot_settle = events
        .iter()
        .find(|e| e["tool"] == "reboot" && e["status"] == "ok" && e["result"]["ok"] == true)
        .expect("reboot ok settle");
    assert_eq!(reboot_settle["result"]["run_id"], json!(reboot_run_id));
    // A refused settle summarizes refused:true (and attaches nothing).
    let refused_settle = events
        .iter()
        .find(|e| e["tool"] == "deploy" && e["status"] == "ok" && e["result"]["refused"] == true)
        .expect("deploy refused settle");
    assert_eq!(refused_settle["result"]["run_id"], Value::Null);
    // The reload error settled as an error event carrying the message.
    assert!(
        events.iter().any(|e| e["tool"] == "reload"
            && e["status"] == "error"
            && e["detail"] == "reload unavailable: this host cannot swap the inventory"),
        "reload unavailable must settle as an error event"
    );

    // ---- audit trail (mutating settles, headless) --------------------------
    let audit_path = state.join("mcp").join("audit.jsonl");
    let audit_text = std::fs::read_to_string(&audit_path).expect("audit.jsonl exists");
    let audit: Vec<Value> = audit_text
        .lines()
        .map(|l| serde_json::from_str(l).expect("audit line is JSON"))
        .collect();
    // deploy ×2 (refused + dry), restart_service ×2, reboot ×2, reload ×2
    // (ok + unavailable error) — and nothing else.
    assert_eq!(audit.len(), 8, "audit lines: {audit_text}");
    for line in &audit {
        let tool = line["tool"].as_str().unwrap_or("");
        assert!(
            ["deploy", "reboot", "restart_service", "reload"].contains(&tool),
            "non-mutating tool audited: {line}"
        );
        assert_ne!(line["status"], "start", "start events must not be audited");
        assert!(line["ts"].is_number(), "audit line lacks ts: {line}");
    }
}
