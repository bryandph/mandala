//! Shared follow-mode scroll state for every render-only pane.

use std::ops::Range;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};

/// Maximum retained history for text and terminal-backed panes.
pub const SCROLLBACK_MAX: usize = 8000;

/// A viewport position stored as distance from the content tail.
///
/// Offset-from-tail keeps a following pane pinned at zero. While unpinned,
/// appended lines increase the offset by the same amount so the viewport
/// remains on the material the operator was reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollState {
    offset_from_tail: usize,
    follow: bool,
    content_len: usize,
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            offset_from_tail: 0,
            follow: true,
            content_len: 0,
        }
    }
}

impl ScrollState {
    #[must_use]
    pub fn follow(&self) -> bool {
        self.follow
    }

    #[must_use]
    pub fn offset_from_tail(&self) -> usize {
        self.offset_from_tail
    }

    #[must_use]
    pub fn content_len(&self) -> usize {
        self.content_len
    }

    pub fn update_content(&mut self, new_len: usize) {
        if self.follow {
            self.offset_from_tail = 0;
        } else if new_len > self.content_len {
            self.offset_from_tail = self
                .offset_from_tail
                .saturating_add(new_len - self.content_len);
        }
        self.content_len = new_len;
        self.offset_from_tail = self.offset_from_tail.min(new_len.saturating_sub(1));
        if new_len == 0 {
            self.to_bottom();
        }
    }

    pub fn scroll_up(&mut self, lines: usize, viewport: usize) {
        let maximum = self.content_len.saturating_sub(viewport.max(1));
        self.offset_from_tail = self.offset_from_tail.saturating_add(lines).min(maximum);
        self.follow = self.offset_from_tail == 0;
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.offset_from_tail = self.offset_from_tail.saturating_sub(lines);
        self.follow = self.offset_from_tail == 0;
    }

    pub fn to_top(&mut self, viewport: usize) {
        self.offset_from_tail = self.content_len.saturating_sub(viewport.max(1));
        self.follow = self.offset_from_tail == 0;
    }

    pub fn to_bottom(&mut self) {
        self.offset_from_tail = 0;
        self.follow = true;
    }

    #[must_use]
    pub fn visible_range(&self, viewport: usize) -> Range<usize> {
        let end = self.content_len.saturating_sub(self.offset_from_tail);
        end.saturating_sub(viewport)..end
    }

    /// Paragraph/forest widgets consume an offset from the content top.
    #[must_use]
    pub fn top_offset(&self, viewport: usize) -> u16 {
        self.visible_range(viewport)
            .start
            .try_into()
            .unwrap_or(u16::MAX)
    }
}

/// Reserve the rightmost column for a scrollbar only when the content can
/// actually scroll. Short panes retain their full width.
#[must_use]
pub fn content_area(area: Rect, scroll: &ScrollState) -> Rect {
    if scroll.content_len() > area.height as usize && area.width > 1 {
        Rect {
            width: area.width - 1,
            ..area
        }
    } else {
        area
    }
}

/// Draw a vertical scrollbar whose position follows the same top-of-buffer
/// coordinate used by the sliced paragraph renderers.
pub fn render_scrollbar(
    frame: &mut Frame,
    area: Rect,
    scroll: &ScrollState,
    track_style: Style,
    thumb_style: Style,
) {
    let viewport = area.height as usize;
    if scroll.content_len() <= viewport || area.is_empty() {
        return;
    }
    let mut state = ScrollbarState::new(scroll.content_len())
        .position(scroll.visible_range(viewport).start)
        .viewport_content_length(viewport);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some("│"))
        .thumb_symbol("┃")
        .track_style(track_style)
        .thumb_style(thumb_style);
    frame.render_stateful_widget(scrollbar, area, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_unpin_stability_and_repin() {
        let mut scroll = ScrollState::default();
        scroll.update_content(100);
        assert_eq!(scroll.visible_range(10), 90..100);
        scroll.scroll_up(20, 10);
        assert!(!scroll.follow());
        assert_eq!(scroll.visible_range(10), 70..80);
        scroll.update_content(105);
        assert_eq!(scroll.visible_range(10), 70..80);
        scroll.to_bottom();
        assert!(scroll.follow());
        assert_eq!(scroll.visible_range(10), 95..105);
    }

    #[test]
    fn top_and_cap_shrink_clamp() {
        let mut scroll = ScrollState::default();
        scroll.update_content(50);
        scroll.to_top(8);
        assert_eq!(scroll.visible_range(8), 0..8);
        scroll.update_content(4);
        assert!(scroll.offset_from_tail() < 4);
        assert_eq!(scroll.visible_range(8), 0..1);
        scroll.scroll_down(usize::MAX);
        assert!(scroll.follow());
    }
}
