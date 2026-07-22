use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::sort::display_rows;
use crate::{DerivationStatus, ForestSnapshot};

#[derive(Debug, Clone, Copy, Default)]
pub struct ForestStyles {
    pub unknown: Style,
    pub planned: Style,
    pub building: Style,
    pub transfer: Style,
    pub built: Style,
    pub failed: Style,
    pub summary: Style,
}

pub struct ForestWidget<'a> {
    snapshot: &'a ForestSnapshot,
    styles: ForestStyles,
    scroll: u16,
}

impl<'a> ForestWidget<'a> {
    #[must_use]
    pub fn new(snapshot: &'a ForestSnapshot) -> Self {
        Self {
            snapshot,
            styles: ForestStyles::default(),
            scroll: 0,
        }
    }

    #[must_use]
    pub fn styles(mut self, styles: ForestStyles) -> Self {
        self.styles = styles;
        self
    }

    #[must_use]
    pub fn scroll(mut self, scroll: u16) -> Self {
        self.scroll = scroll;
        self
    }
}

impl Widget for ForestWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut lines = Vec::new();
        for row in display_rows(self.snapshot) {
            let (glyph, style) = match row.node.status {
                DerivationStatus::Unknown => ("?", self.styles.unknown),
                DerivationStatus::Planned => ("○", self.styles.planned),
                DerivationStatus::Building => ("▶", self.styles.building),
                DerivationStatus::Downloading | DerivationStatus::Substituting => {
                    ("↓", self.styles.transfer)
                }
                DerivationStatus::Built => ("✓", self.styles.built),
                DerivationStatus::Failed => ("✗", self.styles.failed.add_modifier(Modifier::BOLD)),
            };
            lines.push(Line::from(vec![
                Span::raw("  ".repeat(row.depth)),
                Span::styled(format!("{glyph} {}", row.node.name), style),
                row.node
                    .eta_seconds
                    .map(|eta| Span::styled(format!(" · ETA ~{eta}s"), style))
                    .unwrap_or_else(|| Span::raw("")),
                row.node
                    .last_activity
                    .map(|text| Span::styled(format!(" — {text}"), style))
                    .unwrap_or_else(|| Span::raw("")),
            ]));
        }
        let c = self.snapshot.counts;
        lines.push(Line::from(Span::styled(
            format!(
                "{} built · {} building · {} waiting · {} failed",
                c.built, c.building, c.planned, c.failed
            ),
            self.styles.summary,
        )));
        Paragraph::new(lines)
            .scroll((self.scroll, 0))
            .render(area, buf);
    }
}
