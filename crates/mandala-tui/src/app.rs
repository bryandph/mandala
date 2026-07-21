//! The runtime half of the `AppState`/`App` split: terminal, channels,
//! timers, subprocess/pty handles, and the single `tokio::select!` loop.
//!
//! Loop shape (design decision, herdr-style hand-rolled):
//! one select over the terminal event stream, the internal channel, and the
//! deadline-min timer set; every wake maps into [`LoopEvent`]; after a wake
//! the already-queued backlog is drained under a fixed budget WITHOUT
//! rendering in between; the frame is drawn only when a handler marked the
//! state dirty.
//!
//! The [`AppState`] transitions stay pure (state.rs / screen.rs); this
//! module maps keys and settle events onto them and owns everything with a
//! handle: the background explorer jobs, the task screens' subprocesses, and
//! the deploy screen's run + nom pane. Screen dismissal continuations are
//! DATA on the screen states (`ConfirmAction`, `after_mutation`) — the
//! runtime reads them here, so there is no callback plumbing.

use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use chrono::Utc;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::{Stream, StreamExt};
use mandala_core::drift;
use mandala_core::registry;
use mandala_core::runner::{DeployRun, ansible_dir};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::Rect;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::context::{
    TuiContext, spawn_activity_pump, spawn_context_eval_expected, spawn_context_load,
    spawn_role_watch,
};
use crate::deploy::DeployJob;
use crate::event::{AppEvent, Deadlines, LoopEvent, TimerId};
use crate::explorer::{ExplorerConfig, pump_lines, spawn_eval_expected, spawn_load, spawn_survey};
use crate::render::render;
use crate::screen::{
    self, AttachedLogState, ConfirmAction, ConfirmState, DeployTab, RebootState, ScreenState,
    TaskState,
};
use crate::state::{AppState, ContextRole, LoadRequest, McpFollowUp, Tab};
use crate::term::TerminalGuard;

/// How many already-queued INTERNAL events one wake may consume before the
/// loop gets a chance to render — a flood (subprocess output, activity
/// events) coalesces into one frame instead of rendering per-event.
const DRAIN_BUDGET: usize = 64;

/// One spinner frame per 100ms — the Python `set_interval(0.1, self._tick)`.
const SPINNER_INTERVAL: Duration = Duration::from_millis(100);

/// The deploy screen's poll cadence (`set_interval(0.25, self._tick)`).
const DEPLOY_POLL: Duration = Duration::from_millis(250);

/// The attached-log screen's poll cadence (`set_interval(0.5, self._pump)`).
const ATTACHED_POLL: Duration = Duration::from_millis(500);

/// Operator actions above plain navigation — the explicit Action enum of
/// the design's loop decision. Each variant computes the target
/// (selection-else-cursor) and pushes its screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// `p`: ansible ad-hoc ping of the selection (TaskScreen).
    Ping,
    /// `R`: reboot the selection behind options + availability pre-check.
    Reboot,
    /// `D`: deploy the selection behind confirm (DeployScreen).
    Deploy,
}

/// What one select wake produced, before mapping into [`LoopEvent`]s.
enum Wake {
    Term(Event),
    App(AppEvent),
    Deadline,
    /// The terminal event stream ended (terminal gone) — treated as quit.
    Closed,
}

/// The runtime handle for one task screen's subprocess. The pump task owns
/// the child (drain lines → wait → settle event); esc terminates via the
/// recorded pid, exactly like `DeployRun::terminate`.
struct TaskJob {
    task_id: u64,
    pid: Option<u32>,
    exited: bool,
}

/// The runtime: everything with a handle lives here, never in
/// [`AppState`].
pub struct App {
    pub state: AppState,
    /// Present when the app owns a real terminal; `None` under tests.
    /// Enables ctrl-z suspend-to-shell.
    pub guard: Option<TerminalGuard>,
    /// The standalone deploy screen's exit code (`run_deploy` reads it
    /// after the loop; the Python `app.exit(returncode)`).
    pub exit_code: Option<i64>,
    cfg: ExplorerConfig,
    dirty: bool,
    quit: bool,
    deadlines: Deadlines,
    tx: mpsc::Sender<AppEvent>,
    rx: mpsc::Receiver<AppEvent>,
    task: Option<TaskJob>,
    deploy: Option<DeployJob>,
    next_task_id: u64,
    /// The joined fleet context (section 6): `Some` routes every eval-class
    /// explorer read through the leader's warm evaluator; `None` is the
    /// local-eval fallback shape.
    context: Option<TuiContext>,
}

impl App {
    pub fn new(state: AppState, cfg: ExplorerConfig) -> Self {
        let (tx, rx) = mpsc::channel(256);
        Self {
            state,
            guard: None,
            exit_code: None,
            cfg,
            dirty: true,
            quit: false,
            deadlines: Deadlines::default(),
            tx,
            rx,
            task: None,
            deploy: None,
            next_task_id: 0,
            context: None,
        }
    }

    /// Sender for background tasks feeding the internal channel.
    pub fn sender(&self) -> mpsc::Sender<AppEvent> {
        self.tx.clone()
    }

    /// Adopt a joined fleet context: record the role + self-filter identity
    /// in state, start the activity pump (ONE pipeline whether leader or
    /// observer) and the role watcher, and route eval-class reads through
    /// the session from here on.
    pub fn adopt_context(&mut self, ctx: TuiContext) {
        self.state.mcp_client = Some(ctx.client_name.clone());
        self.state.context_role = Some(if ctx.leader {
            ContextRole::Leader
        } else {
            ContextRole::Observer
        });
        spawn_activity_pump(ctx.session.clone(), self.tx.clone());
        spawn_role_watch(ctx.session.clone(), self.tx.clone(), ctx.leader);
        self.context = Some(ctx);
    }

    /// Orderly context exit, run AFTER the loop returns and BEFORE the
    /// terminal restores (the Python `action_quit` await-the-host-first
    /// ordering): a leader stops accepting, drains in-flight forwarded
    /// calls within `grace`, closes, and releases discovery — followers
    /// detect and re-race; an observer just detaches cleanly.
    pub async fn shutdown_context(&mut self, grace: Duration) {
        if let Some(ctx) = self.context.take() {
            ctx.session.shutdown(grace).await;
        }
    }

    /// Kick the initial aggregate load (the `on_mount` `_load`). Call once
    /// before [`App::run`].
    pub fn start_initial_load(&mut self) {
        if let Some(req) = self.state.request_load() {
            self.start_load(req);
        }
        self.sync_spinner();
    }

    /// Drive the loop until quit. Generic over the backend AND the event
    /// stream so tests can run the real loop on `TestBackend` with a
    /// synthetic stream of key events.
    pub async fn run<B, S>(&mut self, terminal: &mut Terminal<B>, events: &mut S) -> io::Result<()>
    where
        B: Backend,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        S: Stream<Item = io::Result<Event>> + Unpin,
    {
        while !self.quit {
            if self.dirty {
                let state = &self.state;
                let deploy = self.deploy.as_ref();
                terminal
                    .draw(|frame| {
                        render(state, frame);
                        // The nom pane is runtime state (a pty emulator) —
                        // the pure render leaves the build tab empty and the
                        // runtime blits the pane over it.
                        if let (Some(ScreenState::Deploy(view)), Some(job)) =
                            (state.screen.as_ref(), deploy)
                            && view.active == DeployTab::Build
                            && let Ok(nom) = job.nom.lock()
                        {
                            let area = screen::deploy_content_area(frame.area());
                            frame.render_widget(&*nom, area);
                        }
                    })
                    .map_err(io::Error::other)?;
                self.dirty = false;
            }

            let deadline = self.deadlines.next_deadline();
            let wake = tokio::select! {
                maybe = events.next() => match maybe {
                    Some(Ok(ev)) => Wake::Term(ev),
                    Some(Err(e)) => return Err(e),
                    None => Wake::Closed,
                },
                maybe = self.rx.recv() => match maybe {
                    // Can't close: `self.tx` keeps the channel alive.
                    Some(ev) => Wake::App(ev),
                    None => Wake::Closed,
                },
                () = tokio::time::sleep_until(
                    deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600)),
                ), if deadline.is_some() => Wake::Deadline,
            };
            match wake {
                Wake::Term(ev) => self.handle(LoopEvent::Term(ev), terminal).await?,
                Wake::App(ev) => self.handle(LoopEvent::App(ev), terminal).await?,
                Wake::Deadline => {
                    for id in self.deadlines.pop_due(Instant::now()) {
                        self.handle(LoopEvent::Timer(id), terminal).await?;
                    }
                }
                Wake::Closed => self.quit = true,
            }

            // Bounded drain: consume whatever is ALREADY queued on the
            // INTERNAL channel, then fall through to the single render at
            // the top of the loop. The terminal stream is deliberately NOT
            // drained here (the spike did, via `now_or_never`): crossterm's
            // `EventStream` hands its background wake task the waker of
            // whichever poll spawned it, so a poll under `now_or_never`'s
            // NOOP waker leaves the stream waking a dead waker — the loop
            // goes deaf to input once no other source happens to re-poll it
            // (operator-reported post-eval freeze, reproduced on a pty).
            // Terminal events are human-rate; the flood source is `rx`, and
            // `try_recv` registers no waker at all.
            let mut budget = DRAIN_BUDGET;
            while budget > 0 && !self.quit {
                match self.rx.try_recv() {
                    Ok(ev) => {
                        self.handle(LoopEvent::App(ev), terminal).await?;
                        budget -= 1;
                    }
                    Err(_) => break,
                }
            }
        }
        Ok(())
    }

    async fn handle<B: Backend>(
        &mut self,
        event: LoopEvent,
        terminal: &mut Terminal<B>,
    ) -> io::Result<()>
    where
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        match event {
            LoopEvent::Term(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                self.on_key(key.code, key.modifiers, terminal).await?;
            }
            LoopEvent::Term(Event::Resize(width, height)) => {
                // Propagate a resize to the deploy screen's pty pane.
                if let Some(job) = self.deploy.as_ref()
                    && let Ok(mut nom) = job.nom.lock()
                {
                    let area = screen::deploy_content_area(Rect::new(0, 0, width, height));
                    nom.resize(area.height, area.width);
                }
                self.dirty = true;
            }
            LoopEvent::Term(_) => {}
            LoopEvent::Timer(TimerId::SpinnerTick) => {
                if self.state.tick_spinner() {
                    self.dirty = true;
                    // Re-arm only while jobs run: the timer stops once every
                    // job is idle (the Python `_tick` stop condition).
                    if self.state.any_job_running() {
                        self.deadlines
                            .arm(TimerId::SpinnerTick, Instant::now() + SPINNER_INTERVAL);
                    }
                }
            }
            LoopEvent::Timer(TimerId::DeployPoll) => {
                if let Some(job) = self.deploy.as_mut()
                    && let Some(ScreenState::Deploy(view)) = self.state.screen.as_mut()
                {
                    job.tick(view);
                    self.dirty = true;
                    self.deadlines
                        .arm(TimerId::DeployPoll, Instant::now() + DEPLOY_POLL);
                }
            }
            LoopEvent::Timer(TimerId::AttachedLogPoll) => {
                if let Some(ScreenState::AttachedLog(attached)) = self.state.screen.as_mut() {
                    screen::attached_pump(attached);
                    self.dirty = true;
                    self.deadlines
                        .arm(TimerId::AttachedLogPoll, Instant::now() + ATTACHED_POLL);
                }
            }
            LoopEvent::App(ev) => {
                if let Some(follow) = self.on_app_event(ev) {
                    let size = terminal.size().map_err(io::Error::other)?;
                    self.apply_mcp_follow_up(follow, (size.width, size.height))
                        .await;
                }
            }
        }
        Ok(())
    }

    /// A background job settled or progressed. The drift inputs (snapshot
    /// dir + clock) are read HERE, at the runtime edge, so the state
    /// transitions stay pure. Returns a context follow-up for the caller to
    /// apply (attaching a screen needs the terminal size).
    fn on_app_event(&mut self, ev: AppEvent) -> Option<McpFollowUp> {
        let mut mcp_follow = None;
        let follow_up = match ev {
            AppEvent::LoadFinished { generation, result } => {
                let snapshots = drift::read_snapshots(&drift::state_dir());
                self.state
                    .on_load_finished(generation, result, &snapshots, Utc::now())
            }
            AppEvent::DriftEvalFinished { result } => {
                let snapshots = drift::read_snapshots(&drift::state_dir());
                self.state
                    .on_drift_eval_finished(result, &snapshots, Utc::now())
            }
            AppEvent::SurveyProgress { n } => {
                self.state.on_survey_progress(n);
                None
            }
            AppEvent::SurveyDone { n, rc, error } => {
                let snapshots = drift::read_snapshots(&drift::state_dir());
                self.state
                    .on_survey_done(n, rc, error.as_deref(), &snapshots, Utc::now());
                None
            }
            AppEvent::TaskLine { task_id, line } => {
                if let Some(ScreenState::Task(task)) = self.state.screen.as_mut()
                    && task.task_id == task_id
                {
                    task.push_line(line);
                }
                None
            }
            AppEvent::TaskExited { task_id, rc } => {
                if let Some(job) = self.task.as_mut()
                    && job.task_id == task_id
                {
                    job.exited = true;
                }
                if let Some(ScreenState::Task(task)) = self.state.screen.as_mut()
                    && task.task_id == task_id
                {
                    task.on_exited(rc);
                }
                None
            }
            AppEvent::McpActivity { event } => {
                mcp_follow = self.state.on_mcp_activity(&event);
                None
            }
            AppEvent::McpRoleChanged { leader } => {
                self.state.context_role = Some(if leader {
                    ContextRole::Leader
                } else {
                    ContextRole::Observer
                });
                None
            }
        };
        if let Some(req) = follow_up {
            self.start_load(req);
        }
        self.sync_spinner();
        self.dirty = true;
        mcp_follow
    }

    /// Apply a context-activity follow-up (the imperative tail of the
    /// Python `_on_mcp_activity`, run at the runtime edge).
    async fn apply_mcp_follow_up(&mut self, follow: McpFollowUp, size: (u16, u16)) {
        match follow {
            McpFollowUp::Attach { kind, run_id } => {
                self.attach_run(&kind, run_id.as_deref(), size).await;
            }
            McpFollowUp::DriftLanded => {
                let rev = drift::repo_rev(&self.cfg.flake);
                let state_dir = drift::state_dir();
                let (cached_rev, cached) = drift::load_expected(&state_dir);
                let snapshots = drift::read_snapshots(&state_dir);
                self.state
                    .on_mcp_drift_landed(rev, cached_rev, cached, &snapshots, Utc::now());
            }
            McpFollowUp::ReloadLanded => {
                // The eval already happened at the leader — the queued load
                // re-reads the swapped contract through the context (or
                // re-evaluates locally in the fallback shape), exactly the
                // Python `McpInventorySwap` minus the eval.
                if let Some(req) = self.state.request_reload() {
                    self.start_load(req);
                }
                self.state.set_status("inventory reloaded (mcp)", false);
                self.sync_spinner();
            }
        }
        self.dirty = true;
    }

    /// Attach the matching observer screen to a registry run (`_attach_run`):
    /// with a `run_id` (from the settle's result summary) it's exact; without
    /// one, the newest run of this kind. Only a run whose recorded pid is
    /// alive attaches (a refused call launches nothing but still settles ok);
    /// a run never attaches twice; and — a single-screen-slot adaptation —
    /// an already-open screen is never clobbered.
    async fn attach_run(&mut self, kind: &str, run_id: Option<&str>, size: (u16, u16)) {
        if self.state.screen.is_some() {
            return;
        }
        let info = match run_id {
            Some(id) => registry::open_run(id).map(|obs| obs.info),
            None => registry::list_runs()
                .into_iter()
                .find(|info| info.kind() == kind),
        };
        let Some(info) = info else {
            return;
        };
        if !registry::pid_alive(info.pid()) {
            return;
        }
        if self.state.attached_runs.contains(&info.run_id) {
            return;
        }
        self.state.attached_runs.insert(info.run_id.clone());
        if kind == "deploy" {
            if let Some(run) = DeployRun::attach(&info.run_id) {
                // Same continuation as an operator deploy: refresh drift
                // once the run completes and its screen closes.
                self.start_deploy(run, false, true, true, size).await;
            }
            return;
        }
        let limit = info.meta.get("limit").and_then(Value::as_str).unwrap_or("");
        let title = format!("{kind} {limit}").trim().to_string();
        self.push_attached_log(title, info.run_id.clone(), true);
    }

    async fn on_key<B: Backend>(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        terminal: &mut Terminal<B>,
    ) -> io::Result<()>
    where
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        // Global keys work everywhere, screens or not.
        match code {
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
                return Ok(());
            }
            KeyCode::Char('z') if modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(guard) = self.guard.as_mut() {
                    guard.suspend_to_shell()?;
                    // The shell scribbled over our screen while we slept.
                    terminal.clear().map_err(io::Error::other)?;
                    self.dirty = true;
                }
                return Ok(());
            }
            _ => {}
        }

        if self.state.screen.is_some() {
            let size = terminal.size().map_err(io::Error::other)?;
            return self.on_screen_key(code, (size.width, size.height)).await;
        }

        match code {
            KeyCode::Char('q') => self.quit = true,

            // -- tabs -----------------------------------------------------
            KeyCode::Tab => {
                self.state.tab = self.state.tab.next();
                self.dirty = true;
            }
            KeyCode::BackTab => {
                self.state.tab = self.state.tab.prev();
                self.dirty = true;
            }
            KeyCode::Char('1') => {
                self.state.tab = Tab::Members;
                self.dirty = true;
            }
            KeyCode::Char('2') => {
                self.state.tab = Tab::Groups;
                self.dirty = true;
            }
            KeyCode::Char('3') => {
                self.state.tab = Tab::Drift;
                self.dirty = true;
            }

            // -- select-table semantics (select_table.py) -----------------
            KeyCode::Up | KeyCode::Down => {
                let delta = if code == KeyCode::Up { -1 } else { 1 };
                let table = self.state.active_table_mut();
                if modifiers.contains(KeyModifiers::SHIFT) {
                    table.extend(delta);
                } else if modifiers.contains(KeyModifiers::CONTROL) {
                    table.skip(delta);
                } else {
                    table.move_cursor(delta);
                }
                self.dirty = true;
            }
            KeyCode::Char('k') => {
                self.state.active_table_mut().move_cursor(-1);
                self.dirty = true;
            }
            KeyCode::Char('j') => {
                self.state.active_table_mut().move_cursor(1);
                self.dirty = true;
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                let delta = if code == KeyCode::PageUp { -10 } else { 10 };
                self.state.active_table_mut().move_cursor(delta);
                self.dirty = true;
            }
            KeyCode::Char(' ') => {
                self.state.active_table_mut().toggle();
                self.dirty = true;
            }
            KeyCode::Esc => {
                self.state.active_table_mut().clear_selection();
                self.dirty = true;
            }

            // -- data refresh ---------------------------------------------
            KeyCode::Char('r') => {
                if let Some(req) = self.state.request_reload() {
                    self.start_load(req);
                }
                self.sync_spinner();
                self.dirty = true;
            }
            KeyCode::Char('S') => {
                let (eval, survey) = self.state.refresh_drift();
                if eval {
                    self.start_eval_expected();
                }
                if survey {
                    self.start_survey();
                }
                self.sync_spinner();
                self.dirty = true;
            }

            // -- action tier (pushed screens; views stay read-only) -------
            KeyCode::Char('p') => self.dispatch(Action::Ping),
            KeyCode::Char('R') => self.dispatch(Action::Reboot),
            KeyCode::Char('D') => self.dispatch(Action::Deploy),

            // -- mcp activity panel (`--debug-mcp` only: without the flag
            // the binding does not exist — the key is inert and the footer
            // never hints it, the `check_action` mechanism) ----------------
            KeyCode::Char('m') if self.state.debug_mcp => {
                self.state.mcp_panel = !self.state.mcp_panel;
                self.dirty = true;
            }
            _ => {}
        }
        Ok(())
    }

    /// Keys while a screen is up. Modals are keyboard-driven exactly like
    /// the Python bindings; task/attached/deploy close on esc/q with their
    /// per-screen dismissal semantics.
    async fn on_screen_key(&mut self, code: KeyCode, size: (u16, u16)) -> io::Result<()> {
        match self.state.screen.as_mut().expect("screen present") {
            // ConfirmScreen: y confirm, esc/n cancel.
            ScreenState::Confirm(_) => match code {
                KeyCode::Char('y') => {
                    let Some(ScreenState::Confirm(confirm)) = self.state.screen.take() else {
                        unreachable!("matched Confirm above");
                    };
                    self.dirty = true;
                    match confirm.action {
                        ConfirmAction::Deploy { target } => {
                            let mut run = DeployRun::new(target);
                            run.flake = self.cfg.flake.clone();
                            if let Some(program) = self.cfg.deploy_program.clone() {
                                run.program = Some(program);
                            }
                            self.start_deploy(run, false, true, false, size).await;
                        }
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') => {
                    self.state.screen = None;
                    self.dirty = true;
                }
                _ => {}
            },

            // RebootScreen: 1/2/3 order, d drain, y run, esc/n cancel.
            ScreenState::Reboot(reboot) => match code {
                KeyCode::Char(key @ ('1' | '2' | '3')) => {
                    reboot.set_order(key);
                    self.dirty = true;
                }
                KeyCode::Char('d') => {
                    reboot.toggle_drain();
                    self.dirty = true;
                }
                KeyCode::Char('y') => {
                    let Some(ScreenState::Reboot(reboot)) = self.state.screen.take() else {
                        unreachable!("matched Reboot above");
                    };
                    let choice = reboot.choice();
                    // The chosen order + drain ride as extra-vars through
                    // `reboot_argv` (shared with the MCP tool — the
                    // wrapper-preference rationale lives there).
                    match (self.cfg.reboot_argv)(&reboot.target, choice.serial, choice.drain) {
                        None => self.state.set_status(screen::REBOOT_UNAVAILABLE, false),
                        Some(argv) => {
                            let title = format!("reboot {}", reboot.target);
                            self.push_task(title, argv, ansible_dir(), true);
                        }
                    }
                    self.dirty = true;
                }
                KeyCode::Esc | KeyCode::Char('n') => {
                    self.state.screen = None;
                    self.dirty = true;
                }
                _ => {}
            },

            // TaskScreen: esc/q terminates a still-running task, then
            // dismisses with the rc (None while running / never launched).
            ScreenState::Task(_) => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
                    let Some(ScreenState::Task(task)) = self.state.screen.take() else {
                        unreachable!("matched Task above");
                    };
                    if let Some(job) = self.task.take()
                        && !job.exited
                        && let Some(pid) = job.pid
                    {
                        let _ = nix::sys::signal::kill(
                            nix::unistd::Pid::from_raw(pid as i32),
                            nix::sys::signal::Signal::SIGTERM,
                        );
                    }
                    self.finish_screen(task.rc, task.after_mutation);
                }
            }

            // AttachedLogScreen: esc/q DETACHES — never terminates; the rc
            // rides the dismissal only once the run has settled.
            ScreenState::AttachedLog(_) => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
                    let Some(ScreenState::AttachedLog(attached)) = self.state.screen.take() else {
                        unreachable!("matched AttachedLog above");
                    };
                    self.deadlines.disarm(TimerId::AttachedLogPoll);
                    let rc = screen::attached_close_rc(&attached.run_id);
                    self.finish_screen(rc, attached.after_mutation);
                }
            }

            // DeployScreen: b/p/s + tab cycling; esc terminates (owned) or
            // detaches (attached), standalone exits the app with the rc.
            ScreenState::Deploy(view) => match code {
                KeyCode::Char('b') => {
                    view.active = DeployTab::Build;
                    self.dirty = true;
                }
                KeyCode::Char('p') => {
                    view.active = DeployTab::Playbook;
                    self.dirty = true;
                }
                // `s` jumps to the summary only once it exists.
                KeyCode::Char('s') if view.summary.is_some() => {
                    view.active = DeployTab::Summary;
                    self.dirty = true;
                }
                KeyCode::Tab => {
                    view.active = screen::cycle_tab(view, 1);
                    self.dirty = true;
                }
                KeyCode::BackTab => {
                    view.active = screen::cycle_tab(view, -1);
                    self.dirty = true;
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    let Some(ScreenState::Deploy(view)) = self.state.screen.take() else {
                        unreachable!("matched Deploy above");
                    };
                    self.deadlines.disarm(TimerId::DeployPoll);
                    let rc = match self.deploy.as_mut() {
                        Some(job) => {
                            // A no-op in attached mode (an observer never
                            // owns the subprocess) or once it has exited.
                            job.run.terminate();
                            job.run.returncode()
                        }
                        None => None,
                    };
                    self.deploy = None; // Drop reaps the nom pane's pty child
                    if view.standalone {
                        // returncode is None if the operator bailed before
                        // it finished (`DeployApp(run).run() or 0`).
                        self.exit_code = Some(rc.unwrap_or(0));
                        self.state.screen = None;
                        self.quit = true;
                    } else {
                        self.finish_screen(rc, view.after_mutation);
                    }
                }
                _ => {}
            },
        }
        Ok(())
    }

    /// A screen dismissed with `rc`. The after-mutation rule: a completed
    /// mutation (rc `Some`) auto-refreshes drift; an operator cancel does
    /// not (the Python `_after_mutation` callback).
    fn finish_screen(&mut self, rc: Option<i64>, after_mutation: bool) {
        self.state.screen = None;
        if after_mutation {
            let (eval, survey) = self.state.after_mutation(rc);
            if eval {
                self.start_eval_expected();
            }
            if survey {
                self.start_survey();
            }
            self.sync_spinner();
        }
        self.dirty = true;
    }

    /// Action dispatch: compute the target (selection-else-cursor) and push
    /// the action's screen. No target → no-op.
    fn dispatch(&mut self, action: Action) {
        let Some(target) = self.state.target() else {
            return;
        };
        match action {
            Action::Ping => {
                let argv = (self.cfg.ping_argv)(&target);
                self.push_task(format!("ping {target}"), argv, ansible_dir(), false);
            }
            Action::Reboot => {
                // Availability pre-check: probe the shared launch line the
                // way `action_reboot` does before showing the modal.
                if (self.cfg.reboot_argv)(&target, "1", true).is_none() {
                    self.state.set_status(screen::REBOOT_UNAVAILABLE, false);
                    self.dirty = true;
                    return;
                }
                self.state.screen = Some(ScreenState::Reboot(RebootState::new(target)));
                self.dirty = true;
            }
            Action::Deploy => {
                self.state.screen = Some(ScreenState::Confirm(ConfirmState::new(
                    format!(
                        "Deploy '{target}'?\n(eval-once batch build, then deploy-rs per host with magic rollback)"
                    ),
                    ConfirmAction::Deploy { target },
                )));
                self.dirty = true;
            }
        }
    }

    /// Push a task screen and launch its subprocess: stdin null, stdout +
    /// stderr merged into one line stream (the Python `stderr=STDOUT`),
    /// output CAPTURED (writing through would shred the alternate screen),
    /// `PYTHONUNBUFFERED=1` + `ANSIBLE_FORCE_COLOR=0`. A launch failure is
    /// surfaced in-pane, never a crash.
    pub fn push_task(
        &mut self,
        title: String,
        argv: Vec<String>,
        cwd: PathBuf,
        after_mutation: bool,
    ) {
        self.next_task_id += 1;
        let task_id = self.next_task_id;
        let mut task = TaskState::new(title, task_id, after_mutation);
        task.push_line(format!("$ {}  (cwd={})", argv.join(" "), cwd.display()));

        if argv.is_empty() {
            task.push_line("failed to launch: empty argv".to_string());
            self.task = Some(TaskJob {
                task_id,
                pid: None,
                exited: true,
            });
            self.state.screen = Some(ScreenState::Task(task));
            self.dirty = true;
            return;
        }
        let mut cmd = tokio::process::Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(&cwd)
            .env("PYTHONUNBUFFERED", "1")
            .env("ANSIBLE_FORCE_COLOR", "0")
            // NEVER inherit stdin: an interactive prompt (ssh, vault,
            // become) would wedge the run silently.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match cmd.spawn() {
            Err(e) => {
                task.push_line(format!("failed to launch: {e}"));
                self.task = Some(TaskJob {
                    task_id,
                    pid: None,
                    exited: true,
                });
            }
            Ok(mut child) => {
                task.launched = true;
                let pid = child.id();
                let tx = self.tx.clone();
                let (line_tx, mut line_rx) = mpsc::channel::<String>(64);
                if let Some(stdout) = child.stdout.take() {
                    tokio::spawn(pump_lines(stdout, line_tx.clone()));
                }
                if let Some(stderr) = child.stderr.take() {
                    tokio::spawn(pump_lines(stderr, line_tx.clone()));
                }
                drop(line_tx);
                // The pump task owns the child: drain, wait, settle.
                tokio::spawn(async move {
                    while let Some(line) = line_rx.recv().await {
                        let _ = tx.send(AppEvent::TaskLine { task_id, line }).await;
                    }
                    let rc = match child.wait().await {
                        Ok(status) => exit_code(status),
                        Err(_) => -1,
                    };
                    let _ = tx.send(AppEvent::TaskExited { task_id, rc }).await;
                });
                self.task = Some(TaskJob {
                    task_id,
                    pid,
                    exited: false,
                });
            }
        }
        self.state.screen = Some(ScreenState::Task(task));
        self.dirty = true;
    }

    /// Push a read-only attached-log screen over a registered run (used by
    /// the section-6 auto-attach; tests drive it directly). Pumps once
    /// immediately, then on the 500ms timer.
    pub fn push_attached_log(&mut self, title: String, run_id: String, after_mutation: bool) {
        let mut attached = AttachedLogState::new(title, run_id, after_mutation);
        screen::attached_pump(&mut attached);
        self.state.screen = Some(ScreenState::AttachedLog(attached));
        self.deadlines
            .arm(TimerId::AttachedLogPoll, Instant::now() + ATTACHED_POLL);
        self.dirty = true;
    }

    /// Push the deploy screen. Owned mode (`attached == false`) starts the
    /// run; attached mode never does (the run was launched elsewhere — this
    /// only tails it). The nixlog sink is wired BEFORE the first poll so
    /// nom sees the build from line one. Returns whether the screen pushed
    /// (an owned launch failure surfaces in the status bar instead).
    pub async fn start_deploy(
        &mut self,
        run: DeployRun,
        standalone: bool,
        after_mutation: bool,
        attached: bool,
        size: (u16, u16),
    ) -> bool {
        let mut job = DeployJob::new(run);
        let pane = screen::deploy_content_area(Rect::new(0, 0, size.0, size.1));
        job.spawn_nom(pane.height, pane.width);
        if !attached {
            job.started_at = Some(std::time::Instant::now());
            if let Err(e) = job.run.start().await {
                self.state
                    .set_status(format!("deploy failed to launch: {e}"), true);
                self.dirty = true;
                return false;
            }
        }
        job.attach_nixlog_sink();
        let mut view = screen::DeployViewState::new(
            job.run.limit.clone(),
            job.run.dry_activate,
            standalone,
            attached,
            after_mutation,
        );
        job.tick(&mut view); // first poll AFTER the sink is attached
        self.state.screen = Some(ScreenState::Deploy(view));
        self.deploy = Some(job);
        self.deadlines
            .arm(TimerId::DeployPoll, Instant::now() + DEPLOY_POLL);
        self.dirty = true;
        true
    }

    /// Number of native build records delivered to the active deploy's nom
    /// sink. Primarily useful to assert attach-before-first-poll ordering.
    #[must_use]
    pub fn deploy_nixlog_lines_seen(&self) -> usize {
        self.deploy.as_ref().map_or(0, DeployJob::nixlog_lines_seen)
    }

    /// Arm the spinner tick when a job just started (never pushing an
    /// already-armed deadline forward — one shared 100ms cadence).
    fn sync_spinner(&mut self) {
        if self.state.any_job_running() && !self.deadlines.is_armed(TimerId::SpinnerTick) {
            self.deadlines
                .arm(TimerId::SpinnerTick, Instant::now() + SPINNER_INTERVAL);
        }
    }

    /// Start an aggregate load: through the context's warm evaluator when
    /// one is joined (with local-eval fallback inside the job), else the
    /// local blocking-pool job.
    fn start_load(&mut self, req: LoadRequest) {
        if let Some(ctx) = &self.context {
            spawn_context_load(self.tx.clone(), ctx.session.clone(), self.cfg.clone(), req);
        } else {
            spawn_load(self.tx.clone(), self.cfg.clone(), req);
        }
    }

    fn start_eval_expected(&mut self) {
        if let Some(ctx) = &self.context {
            spawn_context_eval_expected(
                self.tx.clone(),
                ctx.session.clone(),
                self.cfg.clone(),
                self.state.inventory.clone(),
            );
        } else {
            spawn_eval_expected(
                self.tx.clone(),
                self.cfg.clone(),
                self.state.inventory.clone(),
            );
        }
    }

    fn start_survey(&mut self) {
        spawn_survey(self.tx.clone(), self.cfg.clone());
    }
}

/// Exit code the way Python's `Popen.wait()` reports it: the code, or
/// `-signum` when signalled.
fn exit_code(status: std::process::ExitStatus) -> i64 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .map_or_else(|| i64::from(-status.signal().unwrap_or(0)), i64::from)
}
