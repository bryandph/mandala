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

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use mandala_core::inventory::{Inventory, InventoryError};
use mandala_core::registry::{self, Meta};
use mandala_core::runner::{COMMAND_LOG, DeployRun};
use mandala_mcp::MandalaHandler;
use mandala_mcp::effects::{
    AdhocError, AdhocOutput, CommandLaunch, DeployLaunch, Effects, EvalFailure,
};
use serde_json::{Value, json};

// ---- fixtures + normalization ----------------------------------------------

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../cli/tests/fixtures/mcp")
}

fn load_fixture(name: &str) -> Value {
    let path = fixtures_dir().join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading fixture {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing fixture {name}: {e}"))
}

/// Blank out volatile identifiers on BOTH sides the way
/// `capture_fixtures.py`'s `_norm` does (plus `started_at`/`finished_at`,
/// which the capture left as real floats — the README lists them volatile).
fn normalize(v: &Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, val)| {
                    let nv = match k.as_str() {
                        "run_id" | "events_dir" => json!("<run-id>"),
                        "log" => json!("<state-dir>/runs/<run-id>/output.log"),
                        "elapsed" | "started_at" | "finished_at" => json!(0.0),
                        "ts" => json!(0),
                        "pid" => Value::Null,
                        _ => normalize(val),
                    };
                    (k.clone(), nv)
                })
                .collect(),
        ),
        Value::Array(arr) => Value::Array(arr.iter().map(normalize).collect()),
        other => other.clone(),
    }
}

// ---- the injected fleet (capture_fixtures.py `_inv`) ------------------------

fn base_aggregate() -> Value {
    json!({
        "schemaVersion": 1,
        "members": {
            "web": {
                "platform": "metal",
                "architecture": "x86_64-linux",
                "category": "server",
                "role": "web",
                "tags": ["edge"],
            },
            "cache": {"platform": "metal", "architecture": "x86_64-linux"},
            "router": {"platform": "opnsense"},
        },
        "groups": {"k3s": ["cache", "web"], "gateway": ["router"]},
        "projections": {"deploy": {"nodes": ["cache", "web"]}},
    })
}

fn base_inventory() -> Inventory {
    Inventory::from_value(base_aggregate()).expect("fixture aggregate is valid")
}

// ---- the fake effects (the Python capture's monkeypatch points) -------------

/// Which fake a `launch_command` call performs.
#[derive(Clone, Copy)]
enum CommandFake {
    /// The Python `_FakeRun`: registry dir + a teed log carrying out-paths +
    /// meta `{rc: 0, pid: null}`.
    Build,
    /// The Python real-`CommandRun`-with-`_FakePopen` path: header line, meta
    /// with argv/pid/started_at, then the reaper's `{rc: 0, finished_at}`.
    Reboot,
}

#[derive(Default)]
struct FakeEffects {
    /// `subprocess.run` stand-in for ping / restart_service.
    adhoc: Option<Result<AdhocOutput, AdhocError>>,
    /// `drift.eval_expected` stand-in.
    eval: Option<Result<BTreeMap<String, String>, EvalFailure>>,
    /// `drift.repo_rev` stand-in.
    rev: Option<String>,
    /// The fresh aggregate `reload` evaluates.
    fresh: Option<Value>,
    /// `DeployRun.start` stubbed to `resolve_paths()` (no subprocess).
    fake_deploy: bool,
    /// `CommandRun` stand-in.
    fake_command: Option<CommandFake>,
    /// `shutil.which("ans-reboot")` → found.
    reboot_available: bool,
}

fn epoch_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[async_trait]
impl Effects for FakeEffects {
    async fn fresh_inventory(&self, _flake: &str) -> Result<Inventory, InventoryError> {
        let value = self
            .fresh
            .clone()
            .expect("unexpected fresh_inventory call (no fake aggregate configured)");
        Inventory::from_value(value)
    }

    async fn eval_expected(
        &self,
        _flake: &str,
        _members: &[String],
    ) -> Result<BTreeMap<String, String>, EvalFailure> {
        self.eval
            .clone()
            .expect("unexpected eval_expected call (no fake configured)")
    }

    async fn repo_rev(&self, _flake: &str) -> Option<String> {
        self.rev.clone()
    }

    async fn refresh_snapshots(&self) -> io::Result<i32> {
        panic!("unexpected refresh_snapshots call")
    }

    async fn run_adhoc(&self, _argv: Vec<String>) -> Result<AdhocOutput, AdhocError> {
        self.adhoc
            .clone()
            .expect("unexpected run_adhoc call (no fake configured)")
    }

    async fn launch_deploy(&self, limit: &str, dry_activate: bool) -> io::Result<DeployLaunch> {
        assert!(self.fake_deploy, "unexpected launch_deploy call");
        // The capture stubbed `DeployRun.start` to `resolve_paths()`: a
        // registry run dir is allocated (meta stays `{}` — the `unknown` run
        // in the `deploy_status.list` fixture) but nothing spawns.
        let mut run = DeployRun::new(limit);
        run.dry_activate = dry_activate;
        run.resolve_paths()?;
        Ok(DeployLaunch {
            run_id: run.run_id.clone().expect("resolve_paths sets run_id"),
            events_dir: run
                .events_dir
                .clone()
                .expect("resolve_paths sets events_dir"),
        })
    }

    async fn launch_command(
        &self,
        argv: Vec<String>,
        kind: &str,
        cwd: Option<PathBuf>,
        extra_meta: Meta,
    ) -> io::Result<CommandLaunch> {
        let fake = self.fake_command.expect("unexpected launch_command call");
        let (run_id, run_dir) = registry::new_run_dir()?;
        let log = run_dir.join(COMMAND_LOG);
        let mut meta = Meta::new();
        meta.insert("run_id".to_string(), Value::from(run_id.clone()));
        meta.insert("kind".to_string(), Value::from(kind));
        match fake {
            CommandFake::Build => {
                std::fs::write(
                    &log,
                    "these derivations will be built:\n  /nix/store/x-toplevel.drv\n/nix/store/aaa-nixos-system-web\n",
                )?;
                meta.insert("pid".to_string(), Value::Null);
                meta.insert("rc".to_string(), Value::from(0));
                for (k, v) in extra_meta {
                    meta.insert(k, v);
                }
                registry::write_meta(&run_dir, &meta)?;
            }
            CommandFake::Reboot => {
                let cwd_display = cwd.map_or_else(|| ".".to_string(), |p| p.display().to_string());
                std::fs::write(&log, format!("$ {}  (cwd={cwd_display})\n", argv.join(" ")))?;
                meta.insert("pid".to_string(), Value::from(54321));
                meta.insert("argv".to_string(), Value::from(argv));
                meta.insert("started_at".to_string(), Value::from(epoch_f64()));
                for (k, v) in extra_meta {
                    meta.insert(k, v);
                }
                registry::write_meta(&run_dir, &meta)?;
                // The reaper: `_FakePopen.wait()` returned 0 immediately.
                let mut fields = Meta::new();
                fields.insert("rc".to_string(), Value::from(0));
                fields.insert("finished_at".to_string(), Value::from(epoch_f64()));
                registry::update_meta(&run_dir, fields)?;
            }
        }
        Ok(CommandLaunch {
            run_id,
            log,
            launched: true,
        })
    }

    fn reboot_argv(&self, target: &str, serial: &str, drain: bool) -> Option<Vec<String>> {
        if !self.reboot_available {
            return None;
        }
        // `shutil.which("ans-reboot")` faked truthy: the wrapper argv the
        // real `reboot_argv` builds.
        Some(vec![
            "ans-reboot".to_string(),
            "-l".to_string(),
            target.to_string(),
            "-e".to_string(),
            format!("reboot_serial={serial}"),
            "-e".to_string(),
            format!("drain={}", if drain { "true" } else { "false" }),
        ])
    }
}

// ---- driving the server -----------------------------------------------------

type EventLog = Arc<Mutex<Vec<Value>>>;

fn handler(effects: FakeEffects, events: &EventLog) -> MandalaHandler {
    let sink_log = Arc::clone(events);
    MandalaHandler::with_effects(".", Arc::new(effects))
        .preloaded(base_inventory())
        .with_sink(Arc::new(move |e: &Value| {
            sink_log.lock().unwrap().push(e.clone())
        }))
}

async fn call(h: &MandalaHandler, name: &str, args: Value) -> Result<Value, String> {
    let map = args.as_object().cloned().unwrap_or_default();
    match h.call_tool(name, map).await {
        Ok(res) => Ok(Value::Object(res.structured_content.unwrap_or_default())),
        Err(e) => Err(e.to_string()),
    }
}

fn check(failures: &mut Vec<String>, name: &str, actual: &Value) {
    let fixture = load_fixture(name);
    let (a, f) = (normalize(actual), normalize(&fixture));
    if a != f {
        failures.push(format!(
            "{name}: MISMATCH\n  actual:  {}\n  fixture: {}",
            serde_json::to_string_pretty(&a).unwrap_or_default(),
            serde_json::to_string_pretty(&f).unwrap_or_default(),
        ));
    }
}

/// Run-ids carry microsecond timestamps; space out run allocations so the
/// registry's most-recent-first ordering (which the list fixture encodes) is
/// deterministic.
async fn spacer() {
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
}

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
