//! The runtime half of the `AppState`/`App` split: terminal, channels,
//! timers, and the single `tokio::select!` loop.
//!
//! Loop shape (design decision, herdr-style hand-rolled):
//! one select over the terminal event stream, the internal channel, and the
//! deadline-min timer set; every wake maps into [`LoopEvent`]; after a wake
//! the already-queued backlog is drained under a fixed budget WITHOUT
//! rendering in between; the frame is drawn only when a handler marked the
//! state dirty.
//!
//! The [`AppState`] transitions stay pure (state.rs); this module maps keys
//! and settle events onto them and spawns the background tasks they request
//! (aggregate load, expected eval, state survey — [`crate::explorer`]).

use std::io;
use std::time::Duration;

use chrono::Utc;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::{Stream, StreamExt};
use mandala_core::drift;
use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::event::{AppEvent, Deadlines, LoopEvent, TimerId};
use crate::explorer::{ExplorerConfig, spawn_eval_expected, spawn_load, spawn_survey};
use crate::render::render;
use crate::state::{AppState, LoadRequest, Tab};
use crate::term::TerminalGuard;

/// How many already-queued INTERNAL events one wake may consume before the
/// loop gets a chance to render — a flood (subprocess output, activity
/// events) coalesces into one frame instead of rendering per-event.
const DRAIN_BUDGET: usize = 64;

/// One spinner frame per 100ms — the Python `set_interval(0.1, self._tick)`.
const SPINNER_INTERVAL: Duration = Duration::from_millis(100);

/// Operator actions above plain navigation — the explicit Action enum of
/// the design's loop decision. The read tier stops at the seam: each
/// variant's dispatch is a section-5 screen push, stubbed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// `p`: ansible ad-hoc ping of the selection (section 5 TaskScreen).
    Ping,
    /// `R`: reboot the selection behind options + confirm (section 5).
    Reboot,
    /// `D`: deploy the selection behind confirm (section 5 DeployScreen).
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

/// The runtime: everything with a handle lives here, never in
/// [`AppState`].
pub struct App {
    pub state: AppState,
    /// Present when the app owns a real terminal; `None` under tests.
    /// Enables ctrl-z suspend-to-shell.
    pub guard: Option<TerminalGuard>,
    cfg: ExplorerConfig,
    dirty: bool,
    quit: bool,
    deadlines: Deadlines,
    tx: mpsc::Sender<AppEvent>,
    rx: mpsc::Receiver<AppEvent>,
}

impl App {
    pub fn new(state: AppState, cfg: ExplorerConfig) -> Self {
        let (tx, rx) = mpsc::channel(256);
        Self {
            state,
            guard: None,
            cfg,
            dirty: true,
            quit: false,
            deadlines: Deadlines::default(),
            tx,
            rx,
        }
    }

    /// Sender for background tasks feeding the internal channel.
    pub fn sender(&self) -> mpsc::Sender<AppEvent> {
        self.tx.clone()
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
                terminal
                    .draw(|frame| render(&self.state, frame))
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
                Wake::Term(ev) => self.handle(LoopEvent::Term(ev), terminal)?,
                Wake::App(ev) => self.handle(LoopEvent::App(ev), terminal)?,
                Wake::Deadline => {
                    for id in self.deadlines.pop_due(Instant::now()) {
                        self.handle(LoopEvent::Timer(id), terminal)?;
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
                        self.handle(LoopEvent::App(ev), terminal)?;
                        budget -= 1;
                    }
                    Err(_) => break,
                }
            }
        }
        Ok(())
    }

    fn handle<B: Backend>(&mut self, event: LoopEvent, terminal: &mut Terminal<B>) -> io::Result<()>
    where
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        match event {
            LoopEvent::Term(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                self.on_key(key.code, key.modifiers, terminal)?;
            }
            LoopEvent::Term(Event::Resize(..)) => self.dirty = true,
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
            LoopEvent::App(ev) => self.on_app_event(ev),
        }
        Ok(())
    }

    /// A background job settled or progressed. The drift inputs (snapshot
    /// dir + clock) are read HERE, at the runtime edge, so the state
    /// transitions stay pure.
    fn on_app_event(&mut self, ev: AppEvent) {
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
        };
        if let Some(req) = follow_up {
            self.start_load(req);
        }
        self.sync_spinner();
        self.dirty = true;
    }

    fn on_key<B: Backend>(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        terminal: &mut Terminal<B>,
    ) -> io::Result<()>
    where
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        match code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Char('z') if modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(guard) = self.guard.as_mut() {
                    guard.suspend_to_shell()?;
                    // The shell scribbled over our screen while we slept.
                    terminal.clear().map_err(io::Error::other)?;
                    self.dirty = true;
                }
            }

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

            // -- action tier (section 5 screens; seam only here) ----------
            KeyCode::Char('p') => self.dispatch(Action::Ping),
            KeyCode::Char('R') => self.dispatch(Action::Reboot),
            KeyCode::Char('D') => self.dispatch(Action::Deploy),
            _ => {}
        }
        Ok(())
    }

    /// Action dispatch seam. The read tier computes the target
    /// (selection-else-cursor) and stops: pushing the confirm/task/deploy
    /// screens is section 5; until then an action with no target — or any
    /// action at all — is a no-op.
    fn dispatch(&mut self, action: Action) {
        let Some(_target) = self.state.target() else {
            return;
        };
        match action {
            // Section 5: TaskScreen(ansible <target> -m ping).
            Action::Ping => {}
            // Section 5: RebootScreen options → confirm → reboot_argv run.
            Action::Reboot => {}
            // Section 5: ConfirmScreen → DeployScreen(DeployRun{limit}).
            Action::Deploy => {}
        }
    }

    /// Arm the spinner tick when a job just started (never pushing an
    /// already-armed deadline forward — one shared 100ms cadence).
    fn sync_spinner(&mut self) {
        if self.state.any_job_running() && !self.deadlines.is_armed(TimerId::SpinnerTick) {
            self.deadlines
                .arm(TimerId::SpinnerTick, Instant::now() + SPINNER_INTERVAL);
        }
    }

    fn start_load(&mut self, req: LoadRequest) {
        spawn_load(self.tx.clone(), self.cfg.clone(), req);
    }

    fn start_eval_expected(&mut self) {
        spawn_eval_expected(
            self.tx.clone(),
            self.cfg.clone(),
            self.state.inventory.clone(),
        );
    }

    fn start_survey(&mut self) {
        spawn_survey(self.tx.clone(), self.cfg.clone());
    }
}
