//! The runtime half of the `AppState`/`App` split: terminal, channels,
//! timers, and the single `tokio::select!` loop.
//!
//! Loop shape (design decision, herdr-style hand-rolled):
//! one select over the terminal event stream, the internal channel, and the
//! deadline-min timer set; every wake maps into [`LoopEvent`]; after a wake
//! the already-queued backlog is drained under a fixed budget WITHOUT
//! rendering in between; the frame is drawn only when a handler marked the
//! state dirty.

use std::io;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::{FutureExt, Stream, StreamExt};
use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::event::{AppEvent, Deadlines, LoopEvent, TimerId};
use crate::render::render;
use crate::state::{AppState, Status};
use crate::term::TerminalGuard;

/// How many already-queued events one wake may consume before the loop
/// gets a chance to render — a flood coalesces into one frame instead of
/// rendering per-event.
const DRAIN_BUDGET: usize = 64;

const SPINNER_INTERVAL: Duration = Duration::from_millis(120);

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
    /// Present when the app owns a real terminal (the demo); `None` under
    /// tests. Enables ctrl-z suspend-to-shell.
    pub guard: Option<TerminalGuard>,
    dirty: bool,
    quit: bool,
    deadlines: Deadlines,
    tx: mpsc::Sender<AppEvent>,
    rx: mpsc::Receiver<AppEvent>,
}

impl App {
    pub fn new(state: AppState) -> Self {
        let (tx, rx) = mpsc::channel(256);
        Self {
            state,
            guard: None,
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

            // Bounded drain: consume whatever is ALREADY queued, then fall
            // through to the single render at the top of the loop.
            let mut budget = DRAIN_BUDGET;
            while budget > 0 && !self.quit {
                if let Ok(ev) = self.rx.try_recv() {
                    self.handle(LoopEvent::App(ev), terminal)?;
                    budget -= 1;
                    continue;
                }
                match events.next().now_or_never() {
                    Some(Some(Ok(ev))) => {
                        self.handle(LoopEvent::Term(ev), terminal)?;
                        budget -= 1;
                    }
                    Some(Some(Err(e))) => return Err(e),
                    Some(None) => self.quit = true,
                    None => break,
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
                    self.deadlines
                        .arm(TimerId::SpinnerTick, Instant::now() + SPINNER_INTERVAL);
                }
            }
            LoopEvent::App(AppEvent::WorkFinished(result)) => {
                self.state.status = match result {
                    Ok(()) => Status::Idle,
                    Err(msg) => Status::Error(msg),
                };
                self.deadlines.disarm(TimerId::SpinnerTick);
                self.dirty = true;
            }
        }
        Ok(())
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
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Char('z') if modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(guard) = self.guard.as_mut() {
                    guard.suspend_to_shell()?;
                    // The shell scribbled over our screen while we slept.
                    terminal.clear().map_err(io::Error::other)?;
                    self.dirty = true;
                }
            }
            KeyCode::Char('j') | KeyCode::Down => self.dirty |= self.state.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.dirty |= self.state.move_cursor(-1),
            KeyCode::Char('s') if !matches!(self.state.status, Status::Working(_)) => {
                self.state.status = Status::Working("demo job".into());
                self.deadlines
                    .arm(TimerId::SpinnerTick, Instant::now() + SPINNER_INTERVAL);
                self.dirty = true;
                // The demo background task: settles through the internal
                // channel like any real worker would.
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let _ = tx.send(AppEvent::WorkFinished(Ok(()))).await;
                });
            }
            _ => {}
        }
        Ok(())
    }
}
