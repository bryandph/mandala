//! Pure-data application state — the render-visible half of the strict
//! `AppState`/`App` split (design decision: herdr-style hand-rolled loop).
//!
//! Nothing in here may hold a handle: no terminal, no channels, no child
//! processes, no tasks. Render fns take `&AppState` only, which is what
//! makes the whole visible surface drivable from tests through
//! `TestBackend` without a terminal or a runtime.

/// Spinner frames advanced by the tick timer while a job runs.
pub const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Status-line state. A later section grows this toward the Python
/// explorer's sticky-error semantics; the skeleton keeps the three shapes
/// the render path must distinguish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Nothing running; the status line shows keyhints, dim.
    Idle,
    /// A job is running; the status line carries a spinner frame.
    Working(String),
    /// A failure to keep visible until something clears it.
    Error(String),
}

/// Demo row lifecycle state — a stand-in vocabulary so the style table has
/// something honest to map (the real drift vocabulary arrives in section 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    Fresh,
    Drifted,
    Unknown,
}

impl RowState {
    /// The one-glyph column the demo table shows.
    pub fn glyph(self) -> &'static str {
        match self {
            RowState::Fresh => "✔",
            RowState::Drifted => "≠",
            RowState::Unknown => "?",
        }
    }
}

/// One row of the demo table.
#[derive(Debug, Clone)]
pub struct DemoRow {
    pub name: String,
    pub role: String,
    pub state: RowState,
}

/// The whole render-visible state. Pure data; `Clone` on purpose so tests
/// can fork scenarios cheaply.
#[derive(Debug, Clone)]
pub struct AppState {
    pub rows: Vec<DemoRow>,
    /// Cursor index into `rows` (the demo's only navigation).
    pub cursor: usize,
    /// Current spinner frame index (mod [`SPINNER_FRAMES`]).
    pub spinner_frame: usize,
    pub status: Status,
}

impl AppState {
    /// The honest demo state: a static table + a spinner + a status line.
    pub fn demo() -> Self {
        let row = |name: &str, role: &str, state| DemoRow {
            name: name.into(),
            role: role.into(),
            state,
        };
        Self {
            rows: vec![
                row("alpha", "controller", RowState::Fresh),
                row("beta", "worker", RowState::Drifted),
                row("gamma", "edge", RowState::Unknown),
            ],
            cursor: 0,
            spinner_frame: 0,
            status: Status::Idle,
        }
    }

    /// Advance the spinner. Returns whether anything visible changed (only
    /// while a job runs — an idle tick must not dirty the frame).
    pub fn tick_spinner(&mut self) -> bool {
        if matches!(self.status, Status::Working(_)) {
            self.spinner_frame = self.spinner_frame.wrapping_add(1);
            true
        } else {
            false
        }
    }

    /// Move the cursor by `delta`, clamped. Returns whether it moved.
    pub fn move_cursor(&mut self, delta: isize) -> bool {
        if self.rows.is_empty() {
            return false;
        }
        let max = self.rows.len() - 1;
        let next = self.cursor.saturating_add_signed(delta).min(max);
        let moved = next != self.cursor;
        self.cursor = next;
        moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_spinner_tick_is_not_a_visible_change() {
        let mut s = AppState::demo();
        assert!(!s.tick_spinner());
        s.status = Status::Working("job".into());
        assert!(s.tick_spinner());
        assert_eq!(s.spinner_frame, 1);
    }

    #[test]
    fn cursor_clamps_at_both_ends() {
        let mut s = AppState::demo();
        assert!(!s.move_cursor(-1));
        assert!(s.move_cursor(1));
        assert!(s.move_cursor(10));
        assert_eq!(s.cursor, 2);
        assert!(!s.move_cursor(1));
    }
}
