//! The runtime half of the `AppState`/`App` split: terminal, channels,
//! timers, subprocess handles, and the single `tokio::select!` loop.
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
//! the deploy screen's run. Screen dismissal continuations are
//! DATA on the screen states (`ConfirmAction`, `after_mutation`) — the
//! runtime reads them here, so there is no callback plumbing.

use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use chrono::Utc;
use crossterm::event::{Event, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
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
use crate::hit::{DeployTabTarget, HitMap, Target};
use crate::keymap::{self, Action as KeyAction, Context as KeyContext};
use crate::nom_pane::NomPane;
use crate::render::render_with_theme;
use crate::screen::{
    self, AttachedLogState, ConfirmAction, ConfirmState, DeployTab, RebootState, RunRow, RunsState,
    ScreenState, TaskState,
};
use crate::scroll::ScrollState;
use crate::state::{AppState, ContextRole, LoadRequest, McpFollowUp, Tab};
use crate::term::TerminalGuard;
use crate::theme::Theme;

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

/// The detached-run settlement watch cadence (meta-only reads — cheap).
const RUN_WATCH_POLL: Duration = Duration::from_secs(2);

fn apply_scroll_action(scroll: &mut ScrollState, action: KeyAction, viewport: usize) -> bool {
    match action {
        KeyAction::MoveUp => scroll.scroll_up(1, viewport),
        KeyAction::MoveDown => scroll.scroll_down(1),
        KeyAction::PageUp => scroll.scroll_up(viewport, viewport),
        KeyAction::PageDown => scroll.scroll_down(viewport),
        KeyAction::HalfPageUp => scroll.scroll_up((viewport / 2).max(1), viewport),
        KeyAction::HalfPageDown => scroll.scroll_down((viewport / 2).max(1)),
        KeyAction::Top => scroll.to_top(viewport),
        KeyAction::Bottom => scroll.to_bottom(),
        _ => return false,
    }
    true
}

fn apply_nom_scroll_action(nom: &NomPane, action: KeyAction, viewport: usize) -> bool {
    match action {
        KeyAction::MoveUp => nom.scroll_up(1),
        KeyAction::MoveDown => nom.scroll_down(1),
        KeyAction::PageUp => nom.scroll_up(viewport),
        KeyAction::PageDown => nom.scroll_down(viewport),
        KeyAction::HalfPageUp => nom.scroll_up((viewport / 2).max(1)),
        KeyAction::HalfPageDown => nom.scroll_down((viewport / 2).max(1)),
        KeyAction::Top => nom.scroll_to_top(),
        KeyAction::Bottom => nom.scroll_to_bottom(),
        _ => false,
    }
}

/// Operator actions above plain navigation — the explicit Action enum of
/// the design's loop decision. Each variant computes the target
/// (selection-else-cursor) and pushes its screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FleetAction {
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
    pub theme: Theme,
    /// Present when the app owns a real terminal; `None` under tests.
    /// Enables ctrl-z suspend-to-shell.
    pub guard: Option<TerminalGuard>,
    /// The standalone deploy screen's exit code (`run_deploy` reads it
    /// after the loop; the Python `app.exit(returncode)`).
    pub exit_code: Option<i64>,
    /// A parting notice the standalone entries print AFTER the terminal
    /// restores (e.g. "run continues detached — re-attach with …").
    pub detach_notice: Option<String>,
    /// Runs detached while still live, watched (meta-only, 2s) so the
    /// after-mutation drift refresh still fires when they settle.
    watched_runs: Vec<(String, bool)>,
    cfg: ExplorerConfig,
    dirty: bool,
    hit_map: HitMap,
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
            theme: Theme::default(),
            guard: None,
            exit_code: None,
            detach_notice: None,
            watched_runs: Vec::new(),
            cfg,
            dirty: true,
            hit_map: HitMap::default(),
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

    #[must_use]
    pub fn hit_map(&self) -> &HitMap {
        &self.hit_map
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

    /// Start commit watching only for the context-free fallback shape. A
    /// joined context's leader owns the one watcher/evaluator for every
    /// participant and publishes reload settles through the activity stream.
    pub fn start_fallback_head_watch(&self, initial_head: String) {
        let flake = self.cfg.flake.clone();
        let tx = self.tx.clone();
        drift::spawn_repo_head_watch(flake, initial_head, move |head| {
            let tx = tx.clone();
            async move { tx.send(AppEvent::RepoHeadChanged { head }).await.is_ok() }
        });
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
                let theme = &self.theme;
                let deploy = self.deploy.as_ref();
                let mut hit_map = HitMap::default();
                terminal
                    .draw(|frame| {
                        hit_map = render_with_theme(state, frame, theme);
                        if let (Some(ScreenState::Deploy(view)), Some(job)) =
                            (state.screen.as_ref(), deploy)
                            && view.active == DeployTab::Build
                            && let Ok(nom) = job.nom.lock()
                        {
                            let area = screen::deploy_content_area(frame.area());
                            frame.render_widget(&*nom, area);
                            nom.render_scrollbar(frame, area, theme.chrome, theme.focused_chrome);
                        }
                    })
                    .map_err(io::Error::other)?;
                self.hit_map = hit_map;
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
                Wake::Term(ev) => self.handle(ev.into(), terminal).await?,
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
                self.on_key(key, terminal).await?;
            }
            LoopEvent::Term(Event::Resize(width, height)) => {
                if let Some(job) = self.deploy.as_ref()
                    && let Ok(mut nom) = job.nom.lock()
                {
                    let area = screen::deploy_content_area(Rect::new(0, 0, width, height));
                    nom.resize(area.height, area.width);
                }
                self.dirty = true;
            }
            LoopEvent::Term(_) => {}
            LoopEvent::Mouse(mouse) => {
                let size = terminal.size().map_err(io::Error::other)?;
                self.on_mouse(mouse, (size.width, size.height)).await?;
            }
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
            LoopEvent::Timer(TimerId::RunWatchPoll) => {
                self.poll_watched_runs();
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
        let mut load_settled = false;
        let follow_up = match ev {
            AppEvent::LoadFinished { generation, result } => {
                load_settled = true;
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
            AppEvent::RepoHeadChanged { head } => {
                self.state.set_status(
                    format!(
                        "checkout moved to {} · reloading inventory",
                        drift::short_rev(Some(&head))
                    ),
                    false,
                );
                self.state.request_reload()
            }
        };
        if let Some(req) = follow_up {
            self.start_load(req);
        } else if load_settled && self.state.start_pending_eval_expected() {
            self.start_eval_expected();
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
                if let Some(req) = self.state.request_inventory_swap() {
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
        if self.state.auto_attached_runs.contains(&info.run_id) {
            return;
        }
        self.state.auto_attached_runs.insert(info.run_id.clone());
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

    /// Manual attach from the runs screen: unrestricted — live or settled,
    /// any number of times (the auto-attach once-only set does not apply).
    /// The after-mutation continuation rides only live runs; re-opening a
    /// settled run is pure inspection.
    async fn attach_run_manual(&mut self, row: RunRow, size: (u16, u16)) {
        if row.kind == "deploy" {
            if let Some(run) = DeployRun::attach(&row.run_id) {
                self.start_deploy(run, false, row.live, true, size).await;
            } else {
                self.state
                    .set_status(format!("run {} is gone", row.run_id), true);
            }
            return;
        }
        let title = format!("{} {}", row.kind, row.limit).trim().to_string();
        self.push_attached_log(title, row.run_id, row.live);
    }

    async fn on_key<B: Backend>(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<B>,
    ) -> io::Result<()>
    where
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        // Global keys work everywhere, screens or not.
        match keymap::resolve(KeyContext::Global, key) {
            Some(KeyAction::Quit) => {
                self.quit = true;
                return Ok(());
            }
            Some(KeyAction::Suspend) => {
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

        let context = match self.state.screen.as_ref() {
            None => KeyContext::Explorer,
            Some(ScreenState::Confirm(_)) => KeyContext::Confirm,
            Some(ScreenState::Reboot(_)) => KeyContext::Reboot,
            Some(ScreenState::Task(_)) => KeyContext::Task,
            Some(ScreenState::AttachedLog(_)) => KeyContext::AttachedLog,
            Some(ScreenState::Deploy(view)) if view.kill_armed => KeyContext::DeployKillArmed,
            Some(ScreenState::Deploy(_)) => KeyContext::Deploy,
            Some(ScreenState::Runs(_)) => KeyContext::Runs,
        };
        let action = keymap::resolve(context, key);

        if self.state.screen.is_some() {
            let size = terminal.size().map_err(io::Error::other)?;
            return self
                .on_screen_action(action, (size.width, size.height))
                .await;
        }

        match action {
            Some(KeyAction::Quit) => self.quit = true,
            Some(KeyAction::NextTab) => {
                self.state.tab = self.state.tab.next();
                self.dirty = true;
            }
            Some(KeyAction::PreviousTab) => {
                self.state.tab = self.state.tab.prev();
                self.dirty = true;
            }
            Some(KeyAction::TabOne) => {
                self.state.tab = Tab::Members;
                self.dirty = true;
            }
            Some(KeyAction::TabTwo) => {
                self.state.tab = Tab::Groups;
                self.dirty = true;
            }
            Some(KeyAction::TabThree) => {
                self.state.tab = Tab::Drift;
                self.dirty = true;
            }
            Some(KeyAction::MoveUp) => {
                self.state.active_table_mut().move_cursor(-1);
                self.dirty = true;
            }
            Some(KeyAction::MoveDown) => {
                self.state.active_table_mut().move_cursor(1);
                self.dirty = true;
            }
            Some(KeyAction::ExtendUp) => {
                self.state.active_table_mut().extend(-1);
                self.dirty = true;
            }
            Some(KeyAction::ExtendDown) => {
                self.state.active_table_mut().extend(1);
                self.dirty = true;
            }
            Some(KeyAction::SkipUp) => {
                self.state.active_table_mut().skip(-1);
                self.dirty = true;
            }
            Some(KeyAction::SkipDown) => {
                self.state.active_table_mut().skip(1);
                self.dirty = true;
            }
            Some(KeyAction::PageUp) => {
                self.state.active_table_mut().move_cursor(-10);
                self.dirty = true;
            }
            Some(KeyAction::PageDown) => {
                self.state.active_table_mut().move_cursor(10);
                self.dirty = true;
            }
            Some(KeyAction::HalfPageUp) => {
                self.state.active_table_mut().move_cursor(-5);
                self.dirty = true;
            }
            Some(KeyAction::HalfPageDown) => {
                self.state.active_table_mut().move_cursor(5);
                self.dirty = true;
            }
            Some(KeyAction::Top) => {
                self.state.active_table_mut().move_cursor(isize::MIN);
                self.dirty = true;
            }
            Some(KeyAction::Bottom) => {
                self.state.active_table_mut().move_cursor(isize::MAX);
                self.dirty = true;
            }
            Some(KeyAction::ToggleSelection) => {
                self.state.active_table_mut().toggle();
                self.dirty = true;
            }
            Some(KeyAction::ClearSelection) => {
                self.state.active_table_mut().clear_selection();
                self.dirty = true;
            }
            Some(KeyAction::Reload) => {
                if let Some(req) = self.state.request_reload() {
                    self.start_load(req);
                }
                self.sync_spinner();
                self.dirty = true;
            }
            Some(KeyAction::RefreshDrift) => {
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
            Some(KeyAction::Ping) => self.dispatch(FleetAction::Ping),
            Some(KeyAction::Reboot) => self.dispatch(FleetAction::Reboot),
            Some(KeyAction::Deploy) => self.dispatch(FleetAction::Deploy),
            Some(KeyAction::OpenRuns) => {
                self.state.screen = Some(ScreenState::Runs(RunsState::load()));
                self.dirty = true;
            }
            Some(KeyAction::ToggleMcp) if self.state.debug_mcp => {
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
    async fn on_screen_action(
        &mut self,
        action: Option<KeyAction>,
        size: (u16, u16),
    ) -> io::Result<()> {
        match self.state.screen.as_mut().expect("screen present") {
            // ConfirmScreen: y confirm, esc/n cancel.
            ScreenState::Confirm(confirm) => match action {
                Some(KeyAction::ToggleHalt) => {
                    confirm.halt_on_build_failure = !confirm.halt_on_build_failure;
                    self.dirty = true;
                }
                Some(KeyAction::ToggleBoot) => {
                    confirm.boot = !confirm.boot;
                    self.dirty = true;
                }
                Some(KeyAction::Confirm) => {
                    let Some(ScreenState::Confirm(confirm)) = self.state.screen.take() else {
                        unreachable!("matched Confirm above");
                    };
                    self.dirty = true;
                    match confirm.action {
                        ConfirmAction::Deploy { target } => {
                            let mut run = DeployRun::new(target);
                            run.flake = self.cfg.flake.clone();
                            run.boot = confirm.boot;
                            run.halt_on_build_failure = confirm.halt_on_build_failure;
                            if let Some(program) = self.cfg.deploy_program.clone() {
                                run.program = Some(program);
                            }
                            self.start_deploy(run, false, true, false, size).await;
                        }
                    }
                }
                Some(KeyAction::Cancel) => {
                    self.state.screen = None;
                    self.dirty = true;
                }
                _ => {}
            },

            // RebootScreen: 1/2/3 order, d drain, y run, esc/n cancel.
            ScreenState::Reboot(reboot) => match action {
                Some(KeyAction::OrderOne | KeyAction::OrderTwo | KeyAction::OrderThree) => {
                    let order = match action {
                        Some(KeyAction::OrderOne) => '1',
                        Some(KeyAction::OrderTwo) => '2',
                        Some(KeyAction::OrderThree) => '3',
                        _ => unreachable!(),
                    };
                    reboot.set_order(order);
                    self.dirty = true;
                }
                Some(KeyAction::ToggleDrain) => {
                    reboot.toggle_drain();
                    self.dirty = true;
                }
                Some(KeyAction::Confirm) => {
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
                Some(KeyAction::Cancel) => {
                    self.state.screen = None;
                    self.dirty = true;
                }
                _ => {}
            },

            // TaskScreen: esc/q terminates a still-running task, then
            // dismisses with the rc (None while running / never launched).
            ScreenState::Task(task) => {
                let viewport = size.1.saturating_sub(4) as usize;
                if action
                    .is_some_and(|action| apply_scroll_action(&mut task.scroll, action, viewport))
                {
                    self.dirty = true;
                } else if action == Some(KeyAction::Close) {
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
            ScreenState::AttachedLog(attached) => {
                let viewport = size.1.saturating_sub(4) as usize;
                if action.is_some_and(|action| {
                    apply_scroll_action(&mut attached.scroll, action, viewport)
                }) {
                    self.dirty = true;
                } else if action == Some(KeyAction::Close) {
                    let Some(ScreenState::AttachedLog(attached)) = self.state.screen.take() else {
                        unreachable!("matched AttachedLog above");
                    };
                    self.deadlines.disarm(TimerId::AttachedLogPoll);
                    let rc = screen::attached_close_rc(&attached.run_id);
                    if rc.is_none() && attached.after_mutation {
                        // Detached mid-run: the settlement watch keeps the
                        // after-mutation refresh alive.
                        self.watch_run(attached.run_id.clone(), true);
                    }
                    if attached.standalone {
                        self.exit_code = Some(rc.unwrap_or(0));
                        if rc.is_none() {
                            self.detach_notice = Some(format!(
                                "run {} continues\nre-attach with: mandala tui attach {}",
                                attached.run_id, attached.run_id
                            ));
                        }
                        self.state.screen = None;
                        self.quit = true;
                        return Ok(());
                    }
                    self.finish_screen(rc, attached.after_mutation);
                }
            }

            // DeployScreen: b/p/s + tab cycling; esc ALWAYS detaches (runs
            // are engine-owned and survive their frontends — standalone
            // exits the app with the rc, 0 while the run continues);
            // terminate is the explicit armed ctrl-k → y sequence.
            ScreenState::Deploy(view) if view.kill_armed => {
                view.kill_armed = false;
                if action == Some(KeyAction::ConfirmTerminate) {
                    if let Some(job) = self.deploy.as_mut() {
                        job.run.terminate();
                    }
                    self.state.set_status("terminate signalled", true);
                } else {
                    self.state.set_status("terminate cancelled", false);
                }
                self.dirty = true;
            }
            ScreenState::Deploy(view) => match action {
                Some(KeyAction::ArmTerminate) if !view.finished => {
                    view.kill_armed = true;
                    self.dirty = true;
                }
                Some(
                    action @ (KeyAction::MoveUp
                    | KeyAction::MoveDown
                    | KeyAction::PageUp
                    | KeyAction::PageDown
                    | KeyAction::HalfPageUp
                    | KeyAction::HalfPageDown
                    | KeyAction::Top
                    | KeyAction::Bottom),
                ) => {
                    let viewport = size.1.saturating_sub(7) as usize;
                    let changed = if view.active == DeployTab::Build {
                        self.deploy
                            .as_ref()
                            .and_then(|job| job.nom.lock().ok())
                            .is_some_and(|nom| apply_nom_scroll_action(&nom, action, viewport))
                    } else {
                        view.active_scroll_mut()
                            .is_some_and(|scroll| apply_scroll_action(scroll, action, viewport))
                    };
                    self.dirty |= changed;
                }
                Some(KeyAction::BuildTab) => {
                    view.active = DeployTab::Build;
                    self.dirty = true;
                }
                Some(KeyAction::PlaybookTab) => {
                    view.active = DeployTab::Playbook;
                    self.dirty = true;
                }
                // `s` jumps to the summary only once it exists.
                Some(KeyAction::SummaryTab) if view.summary.is_some() => {
                    view.active = DeployTab::Summary;
                    self.dirty = true;
                }
                Some(KeyAction::NextTab) => {
                    view.active = screen::cycle_tab(view, 1);
                    self.dirty = true;
                }
                Some(KeyAction::PreviousTab) => {
                    view.active = screen::cycle_tab(view, -1);
                    self.dirty = true;
                }
                Some(KeyAction::Close) => {
                    let Some(ScreenState::Deploy(view)) = self.state.screen.take() else {
                        unreachable!("matched Deploy above");
                    };
                    self.deadlines.disarm(TimerId::DeployPoll);
                    // NEVER terminate on dismissal: the run is engine-owned
                    // and keeps going; termination is only the gated verb.
                    let rc = self.deploy.as_mut().and_then(|job| job.run.returncode());
                    let run_id = self.deploy.as_ref().and_then(|job| job.run.run_id.clone());
                    self.deploy = None;
                    if rc.is_none()
                        && let Some(id) = run_id.clone()
                    {
                        // Detached mid-run: keep the after-mutation refresh
                        // alive via the settlement watch.
                        if view.after_mutation {
                            self.watch_run(id.clone(), true);
                        }
                        self.state.set_status(
                            format!("detached — run {id} continues (a: runs list)"),
                            false,
                        );
                    }
                    if view.standalone {
                        // rc is None if the operator detached before it
                        // finished (`DeployApp(run).run() or 0`).
                        self.exit_code = Some(rc.unwrap_or(0));
                        if rc.is_none()
                            && let Some(id) = run_id
                        {
                            self.detach_notice = Some(format!(
                                "deploy continues detached: run {id}\nre-attach with: mandala tui attach {id}"
                            ));
                        }
                        self.state.screen = None;
                        self.quit = true;
                    } else {
                        self.finish_screen(rc, view.after_mutation);
                    }
                }
                _ => {}
            },

            // RunsScreen: navigate, refresh, attach (every registry run is
            // re-attachable — the foreground verb for backgrounded runs).
            ScreenState::Runs(runs) => match action {
                Some(KeyAction::MoveUp) => {
                    runs.move_cursor(-1);
                    self.dirty = true;
                }
                Some(KeyAction::MoveDown) => {
                    runs.move_cursor(1);
                    self.dirty = true;
                }
                Some(KeyAction::PageUp) => {
                    runs.move_cursor(-10);
                    self.dirty = true;
                }
                Some(KeyAction::PageDown) => {
                    runs.move_cursor(10);
                    self.dirty = true;
                }
                Some(KeyAction::HalfPageUp) => {
                    runs.move_cursor(-5);
                    self.dirty = true;
                }
                Some(KeyAction::HalfPageDown) => {
                    runs.move_cursor(5);
                    self.dirty = true;
                }
                Some(KeyAction::Top) => {
                    runs.move_cursor(-(runs.cursor as i64));
                    self.dirty = true;
                }
                Some(KeyAction::Bottom) => {
                    let delta = runs
                        .rows
                        .len()
                        .saturating_sub(1)
                        .saturating_sub(runs.cursor) as i64;
                    runs.move_cursor(delta);
                    self.dirty = true;
                }
                Some(KeyAction::RefreshRuns) => {
                    *runs = RunsState::load();
                    self.dirty = true;
                }
                Some(KeyAction::Activate) => {
                    let Some(ScreenState::Runs(runs)) = self.state.screen.take() else {
                        unreachable!("matched Runs above");
                    };
                    if let Some(row) = runs.selected().cloned() {
                        self.attach_run_manual(row, size).await;
                    }
                    self.dirty = true;
                }
                Some(KeyAction::Close) => {
                    self.state.screen = None;
                    self.dirty = true;
                }
                _ => {}
            },
        }
        Ok(())
    }

    async fn on_mouse(&mut self, mouse: MouseEvent, size: (u16, u16)) -> io::Result<()> {
        let Some(target) = self.hit_map.hit(mouse.column, mouse.row).cloned() else {
            return Ok(());
        };
        if matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            let up = mouse.kind == MouseEventKind::ScrollUp;
            match target {
                Target::ScrollPane { pane, viewport } => {
                    if pane == crate::hit::ScrollPane::Build {
                        let changed = self
                            .deploy
                            .as_ref()
                            .and_then(|job| job.nom.lock().ok())
                            .is_some_and(|nom| {
                                if up {
                                    nom.scroll_up(3)
                                } else {
                                    nom.scroll_down(3)
                                }
                            });
                        self.dirty |= changed;
                    } else {
                        let scroll = match pane {
                            crate::hit::ScrollPane::Mcp => Some(&mut self.state.mcp_scroll),
                            crate::hit::ScrollPane::Task => match self.state.screen.as_mut() {
                                Some(ScreenState::Task(task)) => Some(&mut task.scroll),
                                _ => None,
                            },
                            crate::hit::ScrollPane::AttachedLog => {
                                match self.state.screen.as_mut() {
                                    Some(ScreenState::AttachedLog(attached)) => {
                                        Some(&mut attached.scroll)
                                    }
                                    _ => None,
                                }
                            }
                            crate::hit::ScrollPane::Deploy => match self.state.screen.as_mut() {
                                Some(ScreenState::Deploy(view)) => view.active_scroll_mut(),
                                _ => None,
                            },
                            crate::hit::ScrollPane::Build => unreachable!(),
                        };
                        if let Some(scroll) = scroll {
                            if up {
                                scroll.scroll_up(3, viewport);
                            } else {
                                scroll.scroll_down(3);
                            }
                            self.dirty = true;
                        }
                    }
                }
                Target::ExplorerTable(tab) | Target::ExplorerRow { tab, .. } => {
                    self.state.tab = tab;
                    self.state
                        .active_table_mut()
                        .move_cursor(if up { -3 } else { 3 });
                    self.dirty = true;
                }
                Target::RunsTable | Target::RunsRow(_) => {
                    if let Some(ScreenState::Runs(runs)) = self.state.screen.as_mut() {
                        runs.move_cursor(if up { -3 } else { 3 });
                        self.dirty = true;
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return Ok(());
        }
        match target {
            Target::Action(action) if self.state.screen.is_some() => {
                self.on_screen_action(Some(action), size).await?;
            }
            Target::Action(KeyAction::TabOne) => {
                self.state.tab = Tab::Members;
                self.dirty = true;
            }
            Target::Action(KeyAction::TabTwo) => {
                self.state.tab = Tab::Groups;
                self.dirty = true;
            }
            Target::Action(KeyAction::TabThree) => {
                self.state.tab = Tab::Drift;
                self.dirty = true;
            }
            Target::ExplorerRow { tab, index } => {
                self.state.tab = tab;
                let table = self.state.active_table_mut();
                let delta = index as isize - table.cursor() as isize;
                table.move_cursor(delta);
                table.toggle();
                self.dirty = true;
            }
            Target::RunsRow(index) => {
                if let Some(ScreenState::Runs(runs)) = self.state.screen.as_mut() {
                    let delta = index as i64 - runs.cursor as i64;
                    runs.move_cursor(delta);
                    self.dirty = true;
                }
            }
            Target::DeployTab(target) => {
                if let Some(ScreenState::Deploy(view)) = self.state.screen.as_mut() {
                    view.active = match target {
                        DeployTabTarget::Build => DeployTab::Build,
                        DeployTabTarget::Playbook => DeployTab::Playbook,
                        DeployTabTarget::Summary => DeployTab::Summary,
                        DeployTabTarget::Host(name) => DeployTab::Host(name),
                    };
                    self.dirty = true;
                }
            }
            Target::ExplorerTable(_)
            | Target::RunsTable
            | Target::ScrollPane { .. }
            | Target::Action(_) => {}
        }
        Ok(())
    }

    /// Watch a live run whose screen was dismissed: the after-mutation
    /// drift refresh still fires when it settles (spec: leaving the view
    /// backgrounds the run, the post-settlement refresh survives).
    fn watch_run(&mut self, run_id: String, after_mutation: bool) {
        if self.watched_runs.iter().any(|(id, _)| *id == run_id) {
            return;
        }
        self.watched_runs.push((run_id, after_mutation));
        if !self.deadlines.is_armed(TimerId::RunWatchPoll) {
            self.deadlines
                .arm(TimerId::RunWatchPoll, Instant::now() + RUN_WATCH_POLL);
        }
    }

    /// One meta-only pass over the watched runs: a recorded rc (or a dead
    /// pid) settles the watch and fires the after-mutation continuation; a
    /// pruned run just stops being watched.
    fn poll_watched_runs(&mut self) {
        let mut settled: Vec<(Option<i64>, bool)> = Vec::new();
        self.watched_runs.retain(|(run_id, after_mutation)| {
            let Some(obs) = registry::open_run(run_id) else {
                return false; // pruned
            };
            let rc = obs.info.meta.get("rc").and_then(Value::as_i64);
            if rc.is_none() && registry::pid_alive(obs.info.pid()) {
                return true; // still running
            }
            settled.push((rc, *after_mutation));
            false
        });
        for (rc, after_mutation) in settled {
            if after_mutation {
                let (eval, survey) = self.state.after_mutation(rc);
                if eval {
                    self.start_eval_expected();
                }
                if survey {
                    self.start_survey();
                }
                self.sync_spinner();
                self.dirty = true;
            }
        }
        if !self.watched_runs.is_empty() {
            self.deadlines
                .arm(TimerId::RunWatchPoll, Instant::now() + RUN_WATCH_POLL);
        }
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
    fn dispatch(&mut self, action: FleetAction) {
        let target = match action {
            FleetAction::Deploy => self.state.deploy_target(),
            FleetAction::Ping | FleetAction::Reboot => self.state.target(),
        };
        let Some(target) = target else {
            return;
        };
        match action {
            FleetAction::Ping => {
                let argv = (self.cfg.ping_argv)(&target);
                self.push_task(format!("ping {target}"), argv, ansible_dir(), false);
            }
            FleetAction::Reboot => {
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
            FleetAction::Deploy => {
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
    /// only tails it). Returns whether the screen pushed
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
        let mut view = screen::DeployViewState::new_with_boot(
            job.run.limit.clone(),
            job.run.dry_activate,
            job.run.boot,
            standalone,
            attached,
            after_mutation,
        );
        job.tick(&mut view);
        self.state.screen = Some(ScreenState::Deploy(Box::new(view)));
        self.deploy = Some(job);
        self.deadlines
            .arm(TimerId::DeployPoll, Instant::now() + DEPLOY_POLL);
        self.dirty = true;
        true
    }

    /// Number of structured Nix records delivered to the active deploy's nom
    /// renderer.
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
