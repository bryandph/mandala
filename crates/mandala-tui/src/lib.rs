//! mandala-tui — native fleet TUI substrate (OpenSpec change
//! `mandala-native-tui`, spike tasks 1.3/1.4).
//!
//! What lives here now is the skeleton every later tier builds on, not an
//! explorer port:
//!
//! - [`state`] — the strict pure-data [`state::AppState`]: everything the
//!   render fns may see, and nothing they may touch (no handles, no
//!   channels, no terminal).
//! - [`render`] — render fns over `&AppState` into a ratatui `Frame`; the
//!   AppState→render seam is the testable surface (TestBackend + insta).
//! - [`event`] — the single [`event::LoopEvent`] funnel every source maps
//!   into, plus the deadline-min timer set.
//! - [`app`] — the runtime half: terminal, channels, the one
//!   `tokio::select!` loop with bounded drains and a dirty-flag render
//!   path.
//! - [`term`] — raw-mode/alternate-screen guard, panic-hook restore,
//!   suspend-to-shell.
//! - [`nom_pane`] — `nom --json` hosted on a pane-sized pty, vt100-emulated
//!   into the pane (the `nom.py` port, spike 1.3).

pub mod app;
pub mod event;
pub mod nom_pane;
pub mod render;
pub mod state;
pub mod term;
