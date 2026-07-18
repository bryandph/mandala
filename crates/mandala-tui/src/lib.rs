//! mandala-tui — the native fleet TUI (OpenSpec change `mandala-native-tui`).
//!
//! Section 5 state: the explorer tier (section 4) plus the ACTION TIER and
//! DEPLOY RUNNER at parity with the Python `tui/tasks.py` + `tui/deploy.py`
//! — confirm/reboot modals, task/attached-log screens, the deploy screen
//! with the pty-hosted nom pane, and the standalone `tui deploy` entry.
//! Context/MCP integration is section 6.
//!
//! - [`state`] — the strict pure-data [`state::AppState`]: everything the
//!   render fns may see, and nothing they may touch (no handles, no
//!   channels, no terminal); the explorer transitions live here as PURE
//!   methods returning what background work to start.
//! - [`select`] — the `select_table.py` parity widget state: sticky
//!   toggles ∪ shift-range selection over name-registered rows.
//! - [`render`] — render fns over `&AppState` into a ratatui `Frame`; the
//!   AppState→render seam is the testable surface (TestBackend + insta).
//!   The drift styling maps the CORE vocabulary — one mapping, gated
//!   exhaustive.
//! - [`event`] — the single [`event::LoopEvent`] funnel every source maps
//!   into, plus the deadline-min timer set.
//! - [`app`] — the runtime half: terminal, channels, the one
//!   `tokio::select!` loop with bounded drains and a dirty-flag render
//!   path.
//! - [`explorer`] — [`explorer::run_explorer`] and the background jobs
//!   (aggregate load, expected eval, output-captured state survey).
//! - [`term`] — raw-mode/alternate-screen guard, panic-hook restore,
//!   suspend-to-shell.
//! - [`nom_pane`] — `nom --json` hosted on a pane-sized pty, vt100-emulated
//!   into the pane (the `nom.py` port, spike 1.3; wired into the deploy
//!   screen's build tab).
//! - [`ansi`] — the `render.py` CSI/C0 pre-filter + SGR→spans conversion
//!   every streamed pane line goes through.
//! - [`screen`] — the action tier's pushed screens as pure data + render
//!   fns (`tasks.py` + the `deploy.py` view half); dismissal continuations
//!   are data, not callbacks.
//! - [`deploy`] — the deploy screen's runtime ([`deploy::DeployJob`]) and
//!   the standalone [`deploy::run_deploy`] entry.

pub mod ansi;
pub mod app;
pub mod deploy;
pub mod event;
pub mod explorer;
pub mod nom_pane;
pub mod render;
pub mod screen;
pub mod select;
pub mod state;
pub mod term;
