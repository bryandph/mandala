//! mandala-interop-helper — the direction-B half of the fleet-state-formats
//! interop gate (OpenSpec change `mandala-rust-rewrite`, task 2.5).
//!
//! A tiny driver over the REAL Rust runners (`CommandRun`, `DeployRun` with
//! its `program` override, `drift::save_expected`) so the Python test suite
//! (`cli/tests/test_interop_rs.py`) can make the Rust implementation
//! produce registry runs against `MANDALA_FLEET_STATE` and then attach to
//! them with `registry.open_run` / `DeployRun.attach`. Deliberately NOT a
//! `mandala` subcommand: the user-facing CLI surface must not grow for
//! tests — the nix package installs this under `libexec/`, and the
//! `mandala-interop` flake check points `MANDALA_RS_INTEROP_BIN` at it.
//!
//! Payload commands are always trivial (`sh -c 'echo …'`) — never ansible,
//! nix, or the network.
//!
//! Subcommands:
//! * `command-run <kind> <argv…>` — run a [`CommandRun`], print one JSON
//!   line `{run_id, run_dir, log, launched}` immediately (so the caller can
//!   observe the run LIVE), then wait for the reaper to record `rc`.
//! * `deploy-run <limit> <scenario>` — run a [`DeployRun`] whose `program`
//!   is this binary's `emit-events <scenario>` (a genuine Rust event
//!   writer feeding `$MANDALA_FLEET_EVENTS`), print `{run_id, events_dir}`,
//!   then wait for the child to exit.
//! * `emit-events <scenario>` — write event JSONL (serde_json, the same
//!   compact shape the ansible plugins emit) into `$MANDALA_FLEET_EVENTS`.
//! * `save-expected <dir> <rev> <host=toplevel>…` — write the
//!   `.expected.json` cache via [`drift::save_expected`].

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use mandala_core::registry;
use mandala_core::runner::{CommandRun, DeployRun};

fn now_ts() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Print one JSON line and flush — the caller may still be reading while
/// this process keeps running (the live-observation window).
fn emit_info(info: &Value) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{info}");
    let _ = out.flush();
}

/// Append protocol event records to `<dir>/<file>` — one compact JSON
/// object per line, the same field envelope the `mandala.fleet` plugins'
/// `events.Emitter` writes.
fn append_events(dir: &Path, file: &str, records: &[Value]) {
    let mut fh = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(file))
        .expect("open event file");
    for r in records {
        writeln!(fh, "{r}").expect("append event");
    }
}

fn milestone(v: i64, host: &str, name: &str) -> Value {
    json!({"v": v, "ts": now_ts(), "host": host, "plugin": "deploy",
           "event": "milestone", "milestone": name})
}

fn done(v: i64, host: &str, plugin: &str, rc: i64) -> Value {
    json!({"v": v, "ts": now_ts(), "host": host, "plugin": plugin,
           "event": "status", "state": "done", "rc": rc})
}

/// A confirmed host's v1 stream: the milestone chain plus a `done rc=0`.
fn confirmed_stream(host: &str) -> Vec<Value> {
    let mut records: Vec<Value> = ["eval", "build", "copy", "activate", "wait", "confirm"]
        .iter()
        .map(|m| milestone(1, host, m))
        .collect();
    records.push(done(1, host, "deploy", 0));
    records
}

fn emit_events(scenario: &str) {
    let dir = PathBuf::from(
        std::env::var("MANDALA_FLEET_EVENTS").expect("MANDALA_FLEET_EVENTS must be set"),
    );
    match scenario {
        // Two confirmed hosts; alpha's stream carries an unsupported v99
        // record BEFORE its done (readers must skip it and still consume
        // the rest); the build stream completes rc=0 and carries a v2
        // nixlog record.
        "deploy-ok" => {
            let mut alpha: Vec<Value> = ["eval", "build", "copy", "activate", "wait", "confirm"]
                .iter()
                .map(|m| milestone(1, "alpha", m))
                .collect();
            alpha.push(json!({"v": 99, "ts": now_ts(), "host": "alpha",
                              "plugin": "deploy", "event": "line",
                              "line": "future-protocol-noise", "stream": "deploy"}));
            alpha.push(done(1, "alpha", "deploy", 0));
            append_events(&dir, "alpha.jsonl", &alpha);
            append_events(&dir, "beta.jsonl", &confirmed_stream("beta"));
            append_events(
                &dir,
                "controller.jsonl",
                &[
                    json!({"v": 2, "ts": now_ts(), "host": "controller",
                           "plugin": "build", "event": "status",
                           "state": "start", "cmd": ["nix", "build"]}),
                    json!({"v": 2, "ts": now_ts(), "host": "controller",
                           "plugin": "build", "event": "nixlog",
                           "line": "@nix {\"action\":\"start\",\"id\":1}"}),
                    done(2, "controller", "build", 0),
                ],
            );
        }
        // gamma confirms then rolls back (rollback wins over confirmed);
        // delta confirms and a late `done rc=1` must NOT unflag it.
        "deploy-rollback" => {
            let mut gamma = confirmed_stream("gamma");
            gamma.insert(gamma.len() - 1, milestone(1, "gamma", "rollback"));
            append_events(&dir, "gamma.jsonl", &gamma);
            let mut delta: Vec<Value> = ["eval", "build", "copy", "activate", "wait", "confirm"]
                .iter()
                .map(|m| milestone(1, "delta", m))
                .collect();
            delta.push(done(1, "delta", "deploy", 1));
            append_events(&dir, "delta.jsonl", &delta);
        }
        // The batch build dies before any host event exists: liveness must
        // judge FAILED, not unknown.
        "build-death" => {
            append_events(
                &dir,
                "controller.jsonl",
                &[
                    json!({"v": 2, "ts": now_ts(), "host": "controller",
                           "plugin": "build", "event": "status",
                           "state": "start", "cmd": ["nix", "build"]}),
                    done(2, "controller", "build", 2),
                ],
            );
        }
        other => panic!("unknown emit-events scenario: {other}"),
    }
}

async fn command_run(kind: &str, argv: Vec<String>) {
    let mut run = CommandRun::new(argv, kind);
    run.start().await.expect("CommandRun::start");
    let run_dir = run.run_dir.clone().expect("run_dir allocated");
    emit_info(&json!({
        "run_id": run.run_id,
        "run_dir": run_dir,
        "log": run.log_path(),
        "launched": run.launched(),
    }));
    // Stay alive until the reaper records the exit code (the failed-launch
    // path already wrote rc=127) — the caller observes the RUNNING phase
    // through the registry while we wait.
    while !registry::read_meta(&run_dir).contains_key("rc") {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn deploy_run(limit: &str, scenario: &str) {
    let helper = std::env::current_exe().expect("current_exe");
    let mut run = DeployRun::new(limit);
    run.program = Some(vec![
        helper.display().to_string(),
        "emit-events".to_string(),
        scenario.to_string(),
    ]);
    run.start().await.expect("DeployRun::start");
    emit_info(&json!({
        "run_id": run.run_id,
        "events_dir": run.events_dir,
    }));
    while !run.finished() {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn save_expected(dir: &str, rev: &str, pairs: &[String]) {
    let toplevels: BTreeMap<String, String> = pairs
        .iter()
        .map(|p| {
            let (host, toplevel) = p.split_once('=').expect("host=toplevel");
            (host.to_string(), toplevel.to_string())
        })
        .collect();
    mandala_core::drift::save_expected(Some(rev), &toplevels, Path::new(dir))
        .expect("save_expected");
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("command-run") if args.len() >= 4 => {
            command_run(&args[2], args[3..].to_vec()).await;
        }
        Some("deploy-run") if args.len() == 4 => deploy_run(&args[2], &args[3]).await,
        Some("emit-events") if args.len() == 3 => emit_events(&args[2]),
        Some("save-expected") if args.len() >= 4 => save_expected(&args[2], &args[3], &args[4..]),
        _ => {
            eprintln!(
                "usage: mandala-interop-helper command-run <kind> <argv…> | \
                 deploy-run <limit> <scenario> | emit-events <scenario> | \
                 save-expected <dir> <rev> <host=toplevel>…"
            );
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}
