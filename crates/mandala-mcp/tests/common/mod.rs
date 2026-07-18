//! Shared golden-fixture parity harness: the fixture loader, the volatile-
//! field normalization, the injected fleet, and the fake effects — extracted
//! from `tests/parity.rs` so `tests/parity_proxy.rs` replays the SAME
//! scenarios through the context proxy hop (OpenSpec change
//! `mandala-native-tui`, task 3.2). The fakes mirror exactly what the Python
//! capture (`capture_fixtures.py`) monkeypatched; see `parity.rs` for the
//! full provenance notes.

#![allow(dead_code)] // each test binary uses its own subset

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

pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../cli/tests/fixtures/mcp")
}

pub fn load_fixture(name: &str) -> Value {
    let path = fixtures_dir().join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading fixture {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing fixture {name}: {e}"))
}

/// Blank out volatile identifiers on BOTH sides the way
/// `capture_fixtures.py`'s `_norm` does (plus `started_at`/`finished_at`,
/// which the capture left as real floats — the README lists them volatile).
pub fn normalize(v: &Value) -> Value {
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

pub fn base_aggregate() -> Value {
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

pub fn base_inventory() -> Inventory {
    Inventory::from_value(base_aggregate()).expect("fixture aggregate is valid")
}

// ---- the fake effects (the Python capture's monkeypatch points) -------------

/// Which fake a `launch_command` call performs.
#[derive(Clone, Copy)]
pub enum CommandFake {
    /// The Python `_FakeRun`: registry dir + a teed log carrying out-paths +
    /// meta `{rc: 0, pid: null}`.
    Build,
    /// The Python real-`CommandRun`-with-`_FakePopen` path: header line, meta
    /// with argv/pid/started_at, then the reaper's `{rc: 0, finished_at}`.
    Reboot,
}

#[derive(Default)]
pub struct FakeEffects {
    /// `subprocess.run` stand-in for ping / restart_service.
    pub adhoc: Option<Result<AdhocOutput, AdhocError>>,
    /// `drift.eval_expected` stand-in.
    pub eval: Option<Result<BTreeMap<String, String>, EvalFailure>>,
    /// `drift.repo_rev` stand-in.
    pub rev: Option<String>,
    /// The fresh aggregate `reload` evaluates.
    pub fresh: Option<Value>,
    /// `DeployRun.start` stubbed to `resolve_paths()` (no subprocess).
    pub fake_deploy: bool,
    /// `CommandRun` stand-in.
    pub fake_command: Option<CommandFake>,
    /// `shutil.which("ans-reboot")` → found.
    pub reboot_available: bool,
}

pub fn epoch_f64() -> f64 {
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

pub type EventLog = Arc<Mutex<Vec<Value>>>;

pub fn handler(effects: FakeEffects, events: &EventLog) -> MandalaHandler {
    let sink_log = Arc::clone(events);
    MandalaHandler::with_effects(".", Arc::new(effects))
        .preloaded(base_inventory())
        .with_sink(Arc::new(move |e: &Value| {
            sink_log.lock().unwrap().push(e.clone())
        }))
}

pub async fn call(h: &MandalaHandler, name: &str, args: Value) -> Result<Value, String> {
    let map = args.as_object().cloned().unwrap_or_default();
    match h.call_tool(name, map).await {
        Ok(res) => Ok(Value::Object(res.structured_content.unwrap_or_default())),
        Err(e) => Err(e.to_string()),
    }
}

pub fn check(failures: &mut Vec<String>, name: &str, actual: &Value) {
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
pub async fn spacer() {
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
}
