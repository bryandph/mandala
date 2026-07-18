//! The explorer entry point and its background jobs.
//!
//! [`run_explorer`] wires the pure state (state.rs) and the loop (app.rs)
//! to a real terminal. The three background jobs live here as tokio tasks
//! feeding [`AppEvent`]s — never threads, never UI-thread evals:
//!
//! * **aggregate load** — `cli::load_inventory` (the `MANDALA_AGGREGATE_FILE`
//!   seam included) + the drift-cache inspection, off the loop on the
//!   blocking pool; tens of seconds on a real fleet, and blocking the first
//!   paint would mean a gray void (the Python `_load` lesson).
//! * **expected eval** — deploy nodes (evaluating the aggregate itself if
//!   the inventory hasn't landed yet) → `drift::eval_expected` →
//!   `save_expected`, also on the blocking pool.
//! * **state survey** — `ansible-playbook mandala.fleet.state` as a
//!   `tokio::process` child with output CAPTURED (writing through would
//!   shred the alternate screen — the Python survey lesson) and surfaced
//!   only on failure; a live fresh-snapshot tally rides each output line.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use crossterm::event::EventStream;
use mandala_core::runner::ansible_dir;
use mandala_core::{cli, drift};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::mpsc;

use crate::app::App;
use crate::event::AppEvent;
use crate::state::{AppState, LoadRequest, LoadedInventory};
use crate::term::{TerminalGuard, install_panic_hook};

/// What the runtime needs beyond state: the contract to read and the
/// action-tier launch lines. Tests override the argv seams with `sh -c`
/// stubs — never a live fleet, never real ansible/nix/nom.
#[derive(Debug, Clone)]
pub struct ExplorerConfig {
    /// The fleet flake reference (`--flake`).
    pub flake: String,
    /// The survey command (`action_survey`'s argv). The runtime adds cwd
    /// ([`ansible_dir`]) and the `MANDALA_FLEET_STATE` /
    /// `ANSIBLE_FORCE_COLOR=0` environment at spawn time.
    pub survey_argv: Vec<String>,
    /// The `p` ping launch line for a target (the `action_ping` argv).
    /// Default: `ansible <target> -m ping` — deliberately the DEFAULT
    /// stdout callback: `--one-line` AND the oneline/minimal callbacks are
    /// deprecated in core 2.19 (removed 2.23) with no core replacement
    /// (ansible/ansible #85333, closed not-planned), and community
    /// presentation plugins would be a new dependency. The default callback
    /// is the only stable surface; the pane wraps and scrolls.
    pub ping_argv: fn(&str) -> Vec<String>,
    /// The reboot launch line (target, serial, drain) — defaults to the
    /// shared [`mandala_core::runner::reboot_argv`] (wrapper-preference +
    /// availability semantics live there); `None` = reboot unavailable.
    pub reboot_argv: fn(&str, &str, bool) -> Option<Vec<String>>,
    /// Test seam: override the deploy screen's launched argv verbatim
    /// (`DeployRun::program`); `None` builds the real playbook line.
    pub deploy_program: Option<Vec<String>>,
    /// `--debug-mcp`: render the context call-monitoring surface (activity
    /// panel, pending strip, status-bar `mcp <tool>` jobs, `m` toggle).
    /// The activity SUBSCRIPTION is flag-independent — settle events drive
    /// run auto-attach, drift refresh, and reload swaps regardless.
    pub debug_mcp: bool,
}

/// The default `p` argv (see [`ExplorerConfig::ping_argv`]).
fn default_ping_argv(target: &str) -> Vec<String> {
    vec![
        "ansible".to_string(),
        target.to_string(),
        "-m".to_string(),
        "ping".to_string(),
    ]
}

impl Default for ExplorerConfig {
    fn default() -> Self {
        Self {
            flake: ".".to_string(),
            survey_argv: vec![
                "ansible-playbook".to_string(),
                "mandala.fleet.state".to_string(),
            ],
            ping_argv: default_ping_argv,
            reboot_argv: mandala_core::runner::reboot_argv,
            deploy_program: None,
            debug_mcp: false,
        }
    }
}

impl ExplorerConfig {
    /// Config for a flake reference, defaults elsewhere.
    #[must_use]
    pub fn for_flake(flake: impl Into<String>) -> Self {
        Self {
            flake: flake.into(),
            ..Self::default()
        }
    }
}

/// Run the explorer on the real terminal until quit.
///
/// The TUI participates in the checkout's fleet execution context
/// symmetrically (section 6): it joins BEFORE the terminal enters raw mode
/// (degradation notices print normally), claims leadership when no context
/// exists (later `mandala mcp` instances proxy through this process), and
/// otherwise attaches as an observer. On quit the context is shut down
/// FIRST — orderly stop-accept → drain → close → discovery release for a
/// leader, a clean detach for an observer — and only then does the terminal
/// restore (the Python `action_quit` ordering).
///
/// # Errors
/// Terminal setup/IO failures.
pub async fn run_explorer(cfg: ExplorerConfig) -> io::Result<()> {
    install_panic_hook();
    let context = crate::context::join_context(&cfg.flake).await;
    let guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut state = AppState::new();
    state.debug_mcp = cfg.debug_mcp;
    let mut app = App::new(state, cfg);
    app.guard = Some(guard);
    if let Some(ctx) = context {
        app.adopt_context(ctx);
    }
    app.start_initial_load();
    let mut events = EventStream::new();
    let result = app.run(&mut terminal, &mut events).await;
    app.shutdown_context(crate::context::SHUTDOWN_GRACE).await;
    drop(app); // restores the terminal via the guard
    result
}

/// As [`run_explorer`], hosting its own current-thread runtime — the
/// `mandala tui` bin entry (the [`mandala_mcp::serve_stdio_blocking`]
/// pattern: the crate that owns the async entry owns the runtime, so the
/// bin stays runtime-free).
///
/// # Errors
/// Runtime construction and [`run_explorer`] failures.
pub fn run_explorer_blocking(cfg: ExplorerConfig) -> io::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(run_explorer(cfg))
}

// ---- aggregate load ---------------------------------------------------------

/// Load the inventory (seam-aware) and inspect the drift cache beside it —
/// the Python `_load` worker body. The evaluator is QUIET: under the TUI no
/// child may write through the alternate screen (dirty-tree warnings, copy
/// progress — errors still arrive in-band and surface in the status bar).
fn load_and_inspect(flake: &str) -> Result<LoadedInventory, String> {
    let mut evaluator = mandala_core::eval::Evaluator::from_env().quiet();
    let inventory = cli::load_inventory_with(flake, &mut evaluator)?;
    let rev = drift::repo_rev(flake);
    let (cached_rev, cached) = drift::load_expected(&drift::state_dir());
    Ok(LoadedInventory {
        inventory,
        rev,
        cached_rev,
        cached,
    })
}

/// Spawn the aggregate load on the blocking pool; settles as
/// [`AppEvent::LoadFinished`] carrying the request's generation.
pub fn spawn_load(tx: mpsc::Sender<AppEvent>, cfg: ExplorerConfig, req: LoadRequest) {
    tokio::task::spawn_blocking(move || {
        let result = load_and_inspect(&cfg.flake);
        let _ = tx.blocking_send(AppEvent::LoadFinished {
            generation: req.generation,
            result,
        });
    });
}

// ---- expected eval ----------------------------------------------------------

/// Spawn the expected-toplevel eval on the blocking pool; settles as
/// [`AppEvent::DriftEvalFinished`]. Node resolution may force the aggregate
/// eval (an unevaluated inventory) — kept off the loop with the rest of the
/// slow work, exactly like the Python worker.
pub fn spawn_eval_expected(
    tx: mpsc::Sender<AppEvent>,
    cfg: ExplorerConfig,
    inventory: Option<mandala_core::inventory::Inventory>,
) {
    tokio::task::spawn_blocking(move || {
        let result = (|| {
            // One quiet evaluator serves both the fallback aggregate load
            // and the toplevel eval (its worker stays warm across the two).
            let mut evaluator = mandala_core::eval::Evaluator::from_env().quiet();
            let inventory = match inventory {
                Some(inv) => inv,
                None => cli::load_inventory_with(&cfg.flake, &mut evaluator)
                    .map_err(|e| format!("eval failed: {e}"))?,
            };
            let nodes = inventory.deploy_nodes();
            let expected = drift::eval_expected(&mut evaluator, &cfg.flake, &nodes)
                .map_err(|e| format!("eval failed: {e}"))?;
            let rev = drift::repo_rev(&cfg.flake);
            let _ = drift::save_expected(rev.as_deref(), &expected, &drift::state_dir());
            Ok((rev, expected))
        })();
        let _ = tx.blocking_send(AppEvent::DriftEvalFinished { result });
    });
}

// ---- state survey -----------------------------------------------------------

/// Count the snapshots freshly written THIS survey run: non-dot `*.json`
/// files in the state dir with mtime >= `started`. Dot-files are skipped so
/// the `.expected.json` eval cache (rewritten in this same dir by the
/// concurrent eval worker) is never miscounted as a host.
#[must_use]
pub fn fresh_snapshots(directory: &Path, started: SystemTime) -> usize {
    let Ok(read_dir) = std::fs::read_dir(directory) else {
        return 0;
    };
    read_dir
        .filter_map(Result::ok)
        .filter(|entry| {
            let path = entry.path();
            let is_json = path.extension().and_then(|e| e.to_str()) == Some("json");
            let hidden = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'));
            is_json
                && !hidden
                && entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|mtime| mtime >= started)
        })
        .count()
}

/// Spawn the state survey with the runtime's real cwd/state-dir resolution
/// (both at CALL time — the state-dir contract).
pub fn spawn_survey(tx: mpsc::Sender<AppEvent>, cfg: ExplorerConfig) {
    tokio::spawn(run_survey(
        tx,
        cfg.survey_argv,
        ansible_dir(),
        drift::state_dir(),
    ));
}

/// The survey job: run `argv` in `cwd` with `MANDALA_FLEET_STATE` pointed at
/// `state`, output captured, recounting the fresh-snapshot tally on every
/// output line (each line is a cheap cue — and draining the pipes avoids a
/// full-pipe stall). Public so tests can drive it with a stub argv.
pub async fn run_survey(
    tx: mpsc::Sender<AppEvent>,
    argv: Vec<String>,
    cwd: PathBuf,
    state: PathBuf,
) {
    // -1s so a snapshot written in the same clock second as launch still
    // counts as "this run".
    let started = SystemTime::now() - Duration::from_secs(1);
    let Some((program, args)) = argv.split_first() else {
        let _ = tx
            .send(AppEvent::SurveyDone {
                n: 0,
                rc: 1,
                error: Some("failed to launch: empty survey argv".to_string()),
            })
            .await;
        return;
    };
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .current_dir(&cwd)
        .env("MANDALA_FLEET_STATE", &state)
        .env("PYTHONUNBUFFERED", "1")
        .env("ANSIBLE_FORCE_COLOR", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = tx
                .send(AppEvent::SurveyDone {
                    n: 0,
                    rc: 1,
                    error: Some(format!("failed to launch: {e}")),
                })
                .await;
            return;
        }
    };

    // Merge stdout+stderr into one line stream (the Python stderr=STDOUT).
    let (line_tx, mut line_rx) = mpsc::channel::<String>(64);
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(pump_lines(stdout, line_tx.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(pump_lines(stderr, line_tx.clone()));
    }
    drop(line_tx);

    let mut last_line: Option<String> = None;
    let mut last_n: Option<usize> = None;
    while let Some(line) = line_rx.recv().await {
        last_line = Some(line);
        let n = fresh_snapshots(&state, started);
        if last_n != Some(n) {
            last_n = Some(n);
            let _ = tx.send(AppEvent::SurveyProgress { n }).await;
        }
    }

    let rc = match child.wait().await {
        Ok(status) => exit_code(status),
        Err(_) => -1,
    };
    let error = if rc == 0 { None } else { last_line };
    let _ = tx
        .send(AppEvent::SurveyDone {
            n: fresh_snapshots(&state, started),
            rc,
            error,
        })
        .await;
}

/// Forward a pipe's lines into the merged stream (shared with the task
/// screens' subprocess pump).
pub(crate) async fn pump_lines(reader: impl AsyncRead + Unpin, tx: mpsc::Sender<String>) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if tx.send(line).await.is_err() {
            break;
        }
    }
}

/// Exit code the way Python's `Popen.wait()` reports it: the code, or
/// `-signum` when signalled.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|s| -s))
        .unwrap_or(-1)
}
