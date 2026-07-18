//! mandala-tui — the native fleet TUI (OpenSpec change `mandala-native-tui`).
//!
//! Section 4 state: the READ-ONLY explorer tier at parity with the Python
//! `tui/explorer.py` + `tui/select_table.py` — tabs, multi-select tables,
//! the drift dashboard, and the concurrent-jobs status machinery. The
//! action tier (screens/modals/deploy runner) is section 5; context/MCP
//! integration is section 6.
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
//!   into the pane (the `nom.py` port, spike 1.3; consumed in section 5).

pub mod app;
pub mod event;
pub mod explorer;
pub mod nom_pane;
pub mod render;
pub mod select;
pub mod state;
pub mod term;
