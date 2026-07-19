//! Shared parity harness: the injected fleet and the fake effects driving
//! the leader-vs-follower parity suite (`tests/parity_proxy.rs`) and the
//! failover drills (`tests/mcp_failover.rs`). The fakes mirror what the
//! retired Python capture script monkeypatched (subprocess/eval/launch
//! seams) — provenance in git history (the pre-removal Python tree at rev
//! `ab5bba36e1a1ac6fc13f336c06c6f9e485720252`).

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
    AdhocError, AdhocOutput, CommandLaunch, DeployLaunch, Effects, EvalFailure, SurveyOutput,
};
use serde_json::{Value, json};

// ---- the injected fleet -----------------------------------------------------

pub fn base_aggregate() -> Value {
    json!({
        "schemaVersion": 1,
        "members": {
            "web": {
                "name": "web",
                "platform": "metal",
                "architecture": "x86_64-linux",
                "category": "server",
                "role": "web",
                "tags": ["edge"],
            },
            "cache": {"name": "cache", "platform": "metal", "architecture": "x86_64-linux"},
            "router": {"name": "router", "platform": "opnsense"},
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
    /// Read-only survey result for drift refresh tests.
    pub refresh: Option<SurveyOutput>,
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

    async fn refresh_snapshots(&self) -> io::Result<SurveyOutput> {
        Ok(self
            .refresh
            .clone()
            .expect("unexpected refresh_snapshots call"))
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

/// Run-ids carry microsecond timestamps; space out run allocations so the
/// registry's most-recent-first ordering (which the `deploy_status` listing
/// reads observe) is deterministic.
pub async fn spacer() {
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
}
