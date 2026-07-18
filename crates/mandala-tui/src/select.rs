//! File-manager multi-select table state — the `select_table.py` parity port.
//!
//! ```text
//! space          toggle the cursor row in/out of the selection
//! shift+up/down  contiguous range from the anchor (last toggle/cursor)
//! ctrl+up/down   move the cursor WITHOUT touching the selection
//! escape         clear the selection
//! ```
//!
//! Selection is the union of individually toggled rows (sticky) and the
//! current shift-range. Rows are registered by NAME, so the selection
//! survives cursor churn; the render fns draw a marker column from
//! [`SelectTable::is_selected`].
//!
//! Pure data on purpose (the strict `AppState`/`App` split): the widget owns
//! selection + cursor semantics only; row CELLS live beside it in the
//! explorer state, and the viewport window is derived at render time from
//! the cursor — no handles, no ratatui state.

use std::collections::BTreeSet;

/// Selection + cursor state over rows registered by name.
#[derive(Debug, Clone, Default)]
pub struct SelectTable {
    names: Vec<String>,
    /// The applied selection: sticky toggles ∪ the current shift-range.
    selected: BTreeSet<String>,
    /// Individually toggled rows (survive shift-range replacement).
    sticky: BTreeSet<String>,
    /// Range anchor: the last toggle position (or the cursor when a shift
    /// extension starts with no anchor).
    anchor: Option<usize>,
    cursor: usize,
}

impl SelectTable {
    /// Replace the row registry for a refill: clears rows AND selection
    /// state (the Python `reset_rows` behavior — selection never survives a
    /// refill), and resets the cursor to the top (DataTable `clear`).
    pub fn reset_rows(&mut self, names: Vec<String>) {
        self.names = names;
        self.selected = BTreeSet::new();
        self.sticky = BTreeSet::new();
        self.anchor = None;
        self.cursor = 0;
    }

    /// Row count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.names.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// The cursor row index (clamped into the row range by every mutation).
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The name under the cursor (`None` only when there are no rows).
    #[must_use]
    pub fn cursor_name(&self) -> Option<&str> {
        if self.names.is_empty() {
            return None;
        }
        Some(&self.names[self.cursor.min(self.names.len() - 1)])
    }

    /// Selected names in TABLE order (not toggle order).
    #[must_use]
    pub fn selected_names(&self) -> Vec<String> {
        self.names
            .iter()
            .filter(|n| self.selected.contains(*n))
            .cloned()
            .collect()
    }

    /// Whether `name` is in the applied selection (drives the marker column).
    #[must_use]
    pub fn is_selected(&self, name: &str) -> bool {
        self.selected.contains(name)
    }

    /// `space`: toggle the cursor row in/out of the STICKY set, re-anchor at
    /// the cursor, and apply sticky alone — a toggle drops any live
    /// shift-range from the selection (Python `action_toggle_select`).
    pub fn toggle(&mut self) {
        let Some(name) = self.cursor_name().map(str::to_string) else {
            return;
        };
        if !self.sticky.remove(&name) {
            self.sticky.insert(name);
        }
        self.anchor = Some(self.cursor);
        self.selected = self.sticky.clone();
    }

    /// `shift+up/down`: extend the contiguous range from the anchor, MOVING
    /// the cursor; the applied selection becomes sticky ∪ range (each extend
    /// replaces the previous range, never accumulates it).
    pub fn extend(&mut self, delta: isize) {
        if self.names.is_empty() {
            return;
        }
        let anchor = *self.anchor.get_or_insert(self.cursor);
        let row = self.clamped(delta);
        self.cursor = row;
        let (lo, hi) = if anchor <= row {
            (anchor, row)
        } else {
            (row, anchor)
        };
        let mut new = self.sticky.clone();
        new.extend(self.names[lo..=hi].iter().cloned());
        self.selected = new;
    }

    /// `ctrl+up/down`: move the cursor WITHOUT touching selection or anchor.
    pub fn skip(&mut self, delta: isize) {
        if self.names.is_empty() {
            return;
        }
        self.cursor = self.clamped(delta);
    }

    /// `escape`: clear the whole selection (sticky, range, anchor).
    pub fn clear_selection(&mut self) {
        self.sticky = BTreeSet::new();
        self.anchor = None;
        self.selected = BTreeSet::new();
    }

    /// Plain `up`/`down`: cursor movement (the inherited DataTable behavior)
    /// — selection and anchor untouched. Returns whether the cursor moved.
    pub fn move_cursor(&mut self, delta: isize) -> bool {
        if self.names.is_empty() {
            return false;
        }
        let next = self.clamped(delta);
        let moved = next != self.cursor;
        self.cursor = next;
        moved
    }

    /// The cursor moved by `delta`, clamped into `0..len`.
    fn clamped(&self, delta: isize) -> usize {
        let max = self.names.len() - 1;
        self.cursor.saturating_add_signed(delta).min(max)
    }
}

/// The first visible row for a viewport of `height` rows — derived per frame
/// (render fns take `&AppState`, so no offset is stored): the window follows
/// the cursor, pinning it to the last row once past the first page. Minimal
/// but deterministic; proper windowing memory can come with a real need.
#[must_use]
pub fn view_offset(cursor: usize, height: usize) -> usize {
    if height == 0 {
        return cursor;
    }
    (cursor + 1).saturating_sub(height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(names: &[&str]) -> SelectTable {
        let mut t = SelectTable::default();
        t.reset_rows(names.iter().map(|s| (*s).to_string()).collect());
        t
    }

    #[test]
    fn space_toggles_sticky_membership() {
        let mut t = table(&["a", "b", "c"]);
        t.toggle();
        assert_eq!(t.selected_names(), ["a"]);
        t.toggle(); // toggling again removes it
        assert!(t.selected_names().is_empty());
    }

    #[test]
    fn selection_is_sticky_union_current_range_and_toggle_drops_the_range() {
        let mut t = table(&["a", "b", "c", "d"]);
        t.toggle(); // sticky {a}, anchor 0
        t.move_cursor(2); // plain move: anchor stays at the last toggle
        t.extend(1); // range from anchor 0 → row 3: a..d
        assert_eq!(t.selected_names(), ["a", "b", "c", "d"]);
        // Shrinking the range replaces it — never accumulates.
        t.extend(-1);
        assert_eq!(t.selected_names(), ["a", "b", "c"]);
        // A toggle applies sticky ALONE: the live range is dropped.
        t.toggle(); // sticky {a, c}, cursor at 2
        assert_eq!(t.selected_names(), ["a", "c"]);
    }

    #[test]
    fn extend_moves_the_cursor_and_anchors_at_the_start_cursor() {
        let mut t = table(&["a", "b", "c"]);
        t.move_cursor(1);
        t.extend(1); // no anchor yet → anchors at row 1, moves to 2
        assert_eq!(t.cursor(), 2);
        assert_eq!(t.selected_names(), ["b", "c"]);
        // Extending back across the anchor selects the other side.
        t.extend(-1);
        t.extend(-1);
        assert_eq!(t.cursor(), 0);
        assert_eq!(t.selected_names(), ["a", "b"]);
    }

    #[test]
    fn skip_moves_without_touching_the_selection_or_anchor() {
        let mut t = table(&["a", "b", "c"]);
        t.toggle(); // sticky {a}, anchor 0
        t.skip(2);
        assert_eq!(t.cursor(), 2);
        assert_eq!(t.selected_names(), ["a"]);
        // The anchor is still the toggle position: extending ranges from 0.
        t.extend(0);
        assert_eq!(t.selected_names(), ["a", "b", "c"]);
    }

    #[test]
    fn escape_clears_sticky_range_and_anchor() {
        let mut t = table(&["a", "b", "c"]);
        t.toggle();
        t.extend(2);
        assert_eq!(t.selected_names(), ["a", "b", "c"]);
        t.clear_selection();
        assert!(t.selected_names().is_empty());
        // Anchor gone: the next extend anchors at the cursor.
        t.extend(-1);
        assert_eq!(t.selected_names(), ["b", "c"]);
    }

    #[test]
    fn reset_rows_clears_rows_and_selection() {
        let mut t = table(&["a", "b"]);
        t.move_cursor(1);
        t.toggle();
        assert_eq!(t.selected_names(), ["b"]);
        t.reset_rows(vec!["x".into(), "y".into(), "b".into()]);
        // Selection did NOT survive the refill — even for a re-registered name.
        assert!(t.selected_names().is_empty());
        assert_eq!(t.cursor(), 0);
        assert_eq!(t.cursor_name(), Some("x"));
    }

    #[test]
    fn selected_names_come_back_in_table_order() {
        let mut t = table(&["c", "a", "b"]);
        t.skip(2); // cursor on "b"
        t.toggle();
        t.skip(-2); // cursor on "c"
        t.toggle();
        assert_eq!(t.selected_names(), ["c", "b"]);
    }

    #[test]
    fn cursor_name_clamps_and_empty_table_is_inert() {
        let mut t = SelectTable::default();
        assert_eq!(t.cursor_name(), None);
        t.toggle();
        t.extend(1);
        t.skip(1);
        assert!(t.selected_names().is_empty());

        let mut t = table(&["a", "b"]);
        t.move_cursor(10);
        assert_eq!(t.cursor(), 1);
        assert_eq!(t.cursor_name(), Some("b"));
        assert!(!t.move_cursor(1));
        t.extend(5);
        assert_eq!(t.cursor(), 1);
    }

    #[test]
    fn view_offset_follows_the_cursor() {
        assert_eq!(view_offset(0, 5), 0);
        assert_eq!(view_offset(4, 5), 0);
        assert_eq!(view_offset(5, 5), 1); // cursor pinned to the last row
        assert_eq!(view_offset(9, 5), 5);
        assert_eq!(view_offset(3, 0), 3); // degenerate viewport
    }
}
