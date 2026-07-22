//! Deploy-runner runtime: the process half of the deploy screen, and
//! the standalone `mandala tui deploy` entry (tasks 5.4/5.5/5.6).
//!
//! [`DeployJob`] owns or attaches to a [`DeployRun`]; the 250ms loop timer
//! drives [`DeployJob::tick`], which polls the event tailer and refreshes the
//! pure [`DeployViewState`] (including its native build-forest snapshot). The
//! native deploy stays the engine — the argv is built ONLY by
//! `DeployRun`'s own construction (limit guard, throttle, magic rollback
//! never bypassed), and every child's output is captured (the quiet rule).

use std::io;
use std::time::Instant;

use crossterm::event::EventStream;
use mandala_core::runner::DeployRun;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::App;
use crate::explorer::ExplorerConfig;
use crate::screen::DeployViewState;
use crate::state::AppState;
use crate::term::{TerminalGuard, install_panic_hook};

/// The runtime half of the deploy screen. Never rendered directly:
/// [`DeployJob::tick`] snapshots into the pure view state.
pub struct DeployJob {
    pub run: DeployRun,
    /// Owned mode: when the run was launched, for the summary's elapsed
    /// clock. `None` in attached mode (elapsed renders 0 — Python parity).
    pub started_at: Option<Instant>,
}

impl DeployJob {
    #[must_use]
    pub fn new(run: DeployRun) -> Self {
        Self {
            run,
            started_at: None,
        }
    }

    /// Number of structured Nix records accepted by the native forest.
    #[must_use]
    pub fn nixlog_lines_seen(&self) -> usize {
        self.run
            .tailer
            .as_ref()
            .map_or(0, |tailer| tailer.forest.snapshot().version as usize)
    }

    /// One 250ms poll: consume new events and refresh the view state.
    pub fn tick(&mut self, view: &mut DeployViewState) -> bool {
        // Native owned runs do not have a frontend-allocated tailer. Discover
        // the engine-owned run before consuming the first event batch.
        let _ = self.run.discover_run();
        self.run.poll();
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
        false
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
