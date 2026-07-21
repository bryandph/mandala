//! Deploy-runner runtime: the subprocess/pty half of the deploy screen, and
//! the standalone `mandala tui deploy` entry (tasks 5.4/5.5/5.6).
//!
//! [`DeployJob`] pairs the owned/attached [`DeployRun`] with the pty-hosted
//! [`NomPane`]; the 250ms loop timer drives [`DeployJob::tick`], which polls
//! the event tailer, finishes the nom pane exactly once on `build.done`, and
//! refreshes the pure [`DeployViewState`] the render fns consume. The
//! native deploy stays the engine — the argv is built ONLY by
//! `DeployRun`'s own construction (limit guard, throttle, magic rollback
//! never bypassed), and every child's output is captured (the quiet rule).

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crossterm::event::EventStream;
use mandala_core::runner::DeployRun;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::App;
use crate::explorer::ExplorerConfig;
use crate::nom_pane::NomPane;
use crate::screen::DeployViewState;
use crate::state::AppState;
use crate::term::{TerminalGuard, install_panic_hook};

/// The runtime half of the deploy screen: the run (subprocess + tailer) and
/// the nom pane (pty child). Never rendered directly — [`DeployJob::tick`]
/// snapshots into the pure view state; only the nom pane is blitted by the
/// runtime (its emulator screen IS runtime state).
pub struct DeployJob {
    pub run: DeployRun,
    /// Shared with the tailer's nixlog sink (attached BEFORE the first poll
    /// so nom sees the build from line one; the pane's pending buffer covers
    /// lines fed before its pty child spawns).
    pub nom: Arc<Mutex<NomPane>>,
    nom_finished: bool,
    nixlog_attached: bool,
    nixlog_lines_seen: Arc<AtomicUsize>,
    /// Owned mode: when the run was launched, for the summary's elapsed
    /// clock. `None` in attached mode (elapsed renders 0 — Python parity).
    pub started_at: Option<Instant>,
}

impl DeployJob {
    #[must_use]
    pub fn new(run: DeployRun) -> Self {
        Self {
            run,
            nom: Arc::new(Mutex::new(NomPane::new())),
            nom_finished: false,
            nixlog_attached: false,
            nixlog_lines_seen: Arc::new(AtomicUsize::new(0)),
            started_at: None,
        }
    }

    /// Spawn `nom --json` on a pane-sized pty.
    pub fn spawn_nom(&mut self, rows: u16, cols: u16) {
        if let Ok(mut nom) = self.nom.lock() {
            nom.spawn(rows, cols);
        }
    }

    /// Live-wire the internal-json stream into the nom pane. Call after the
    /// tailer exists (post-`start`/`attach`) and BEFORE the first poll.
    pub fn attach_nixlog_sink(&mut self) {
        if self.nixlog_attached {
            return;
        }
        if let Some(tailer) = self.run.tailer.as_mut() {
            let nom = Arc::clone(&self.nom);
            let lines_seen = Arc::clone(&self.nixlog_lines_seen);
            tailer.nixlog_sink = Some(Box::new(move |line| {
                lines_seen.fetch_add(1, Ordering::Relaxed);
                if let Ok(mut nom) = nom.lock() {
                    nom.feed(&line);
                }
            }));
            self.nixlog_attached = true;
        }
    }

    /// Number of native build records delivered to the nom sink.
    #[must_use]
    pub fn nixlog_lines_seen(&self) -> usize {
        self.nixlog_lines_seen.load(Ordering::Relaxed)
    }

    /// One 250ms poll (the `_tick` body): consume new events, EOF the nom
    /// pane exactly once when the batch build completes (nom draws its final
    /// summary), then refresh the view state. Returns whether the pty screen
    /// changed (extra render cue beside the tick's own dirty).
    pub fn tick(&mut self, view: &mut DeployViewState) -> bool {
        // Native owned runs do not have a frontend-allocated tailer. Discover
        // the engine-owned run first, wire nom, and only then consume the first
        // event batch so no early build record can bypass the sink.
        let _ = self.run.discover_run();
        self.attach_nixlog_sink();
        self.run.poll();
        if !self.nom_finished
            && self
                .run
                .tailer
                .as_ref()
                .is_some_and(|tailer| tailer.build.done)
        {
            self.nom_finished = true;
            if let Ok(mut nom) = self.nom.lock() {
                nom.finish();
            }
        }
        let finished = self.run.finished();
        let returncode = self.run.returncode();
        let elapsed = self
            .started_at
            .map_or(0, |started| started.elapsed().as_secs());
        let output = self.run.output();
        view.sync(
            self.run.tailer.as_ref(),
            &output,
            finished,
            returncode,
            elapsed,
        );
        self.nom.lock().map(|nom| nom.take_dirty()).unwrap_or(false)
    }
}

/// What `mandala tui deploy -l <sel> [--dry-activate] [--throttle N]` needs.
#[derive(Debug, Clone)]
pub struct DeployConfig {
    /// The fleet flake reference (selector resolution).
    pub flake: String,
    /// The raw selector: `@group`, member, or comma-list — resolved through
    /// `Inventory::to_limit` (canonical resolution) before anything runs.
    pub limit: String,
    pub dry_activate: bool,
    pub throttle: i64,
    /// Test seam: override the launched argv verbatim (`DeployRun::program`)
    /// — never a real native deploy in tests.
    pub program: Option<Vec<String>>,
}

/// Run the standalone deploy screen on the real terminal. Returns the
/// process exit code: the run's rc, or 0 on operator cancel (the Python
/// `DeployApp(run).run() or 0`).
///
/// # Errors
/// Selector-resolution failures (surfaced before the terminal is touched)
/// and terminal setup/IO failures.
pub async fn run_deploy(cfg: DeployConfig) -> io::Result<i64> {
    // Canonical selector resolution FIRST, outside the alternate screen —
    // an eval error prints normally and nothing launches (the Python
    // `inv.to_limit` before `DeployApp`).
    let flake = cfg.flake.clone();
    let selector = cfg.limit.clone();
    let limit = tokio::task::spawn_blocking(move || {
        let mut evaluator = mandala_core::eval::Evaluator::from_env();
        let inventory = mandala_core::cli::load_inventory_with(&flake, &mut evaluator)
            .map_err(io::Error::other)?;
        inventory.to_limit(&selector).map_err(io::Error::other)
    })
    .await
    .map_err(io::Error::other)??;

    let mut run = DeployRun::new(limit);
    run.flake = cfg.flake.clone();
    run.dry_activate = cfg.dry_activate;
    run.throttle = cfg.throttle;
    run.program = cfg.program.clone();

    install_panic_hook();
    let guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut app = App::new(AppState::new(), ExplorerConfig::for_flake(cfg.flake));
    app.guard = Some(guard);
    let size = terminal.size()?;
    app.start_deploy(run, true, false, false, (size.width, size.height))
        .await;
    let mut events = EventStream::new();
    let result = app.run(&mut terminal, &mut events).await;
    let code = app.exit_code.unwrap_or(0);
    drop(app); // restores the terminal via the guard
    result.map(|()| code)
}

/// As [`run_deploy`], hosting its own current-thread runtime — the
/// `mandala tui deploy` bin entry (the [`mandala_mcp::serve_stdio_blocking`]
/// pattern: the crate that owns the async entry owns the runtime, so the
/// bin stays runtime-free).
///
/// # Errors
/// Runtime construction and [`run_deploy`] failures.
pub fn run_deploy_blocking(cfg: DeployConfig) -> io::Result<i64> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = rt.block_on(run_deploy(cfg));
    // Same bounded teardown as the explorer entry (the 7.4 quit-hang
    // finding): never let a lingering blocking-pool task hold process exit.
    rt.shutdown_timeout(crate::explorer::RUNTIME_TEARDOWN_BOUND);
    result
}
