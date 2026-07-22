use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::sort::{DEFAULT_ACTIVITY_ROW_BUDGET, DisplayRow, activity_projection, display_rows};
use crate::{ActivityType, DerivationStatus, ForestSnapshot, Transfer};

const RENDERED_LOG_LINES: usize = 8;
const RENDERED_TRANSFERS: usize = 4;

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

#[derive(Debug, Clone, Copy)]
enum ForestView {
    Activity { row_budget: usize },
    Full,
}

pub struct ForestWidget<'a> {
    snapshot: &'a ForestSnapshot,
    styles: ForestStyles,
    scroll: u16,
    view: ForestView,
}

impl<'a> ForestWidget<'a> {
    #[must_use]
    pub fn new(snapshot: &'a ForestSnapshot) -> Self {
        Self {
            snapshot,
            styles: ForestStyles::default(),
            scroll: 0,
            view: ForestView::Activity {
                row_budget: DEFAULT_ACTIVITY_ROW_BUDGET,
            },
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

    #[must_use]
    pub fn row_budget(mut self, row_budget: usize) -> Self {
        self.view = ForestView::Activity { row_budget };
        self
    }

    #[must_use]
    pub fn full(mut self) -> Self {
        self.view = ForestView::Full;
        self
    }

    #[must_use]
    pub fn activity_line_count(snapshot: &ForestSnapshot) -> usize {
        render_lines(
            snapshot,
            ForestView::Activity {
                row_budget: DEFAULT_ACTIVITY_ROW_BUDGET,
            },
            ForestStyles::default(),
        )
        .len()
    }
}

impl Widget for ForestWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(render_lines(self.snapshot, self.view, self.styles))
            .scroll((self.scroll, 0))
            .render(area, buf);
    }
}

fn render_lines(
    snapshot: &ForestSnapshot,
    view: ForestView,
    styles: ForestStyles,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    push_logs(snapshot, styles, &mut lines);
    push_transfers(snapshot, styles, &mut lines);

    let (rows, elided) = match view {
        ForestView::Activity { row_budget } => {
            let projection = if row_budget == DEFAULT_ACTIVITY_ROW_BUDGET {
                snapshot.activity.clone()
            } else {
                activity_projection(snapshot, row_budget)
            };
            (projection.rows, projection.elided.total())
        }
        ForestView::Full => (display_rows(snapshot), 0),
    };
    if !lines.is_empty() {
        lines.push(Line::default());
    }
    lines.push(Line::from(Span::styled("activity", styles.summary)));
    for row in rows {
        lines.push(render_row(row, styles));
    }

    let c = snapshot.counts;
    let mut summary = format!(
        "{} built · {} building · {} waiting · {} fetching · {} failed",
        c.built,
        c.building,
        c.planned,
        c.downloading + c.substituting,
        c.failed
    );
    if elided > 0 {
        summary.push_str(&format!(" · {elided} elided"));
    }
    lines.push(Line::from(Span::styled(summary, styles.summary)));
    lines
}

fn push_logs(snapshot: &ForestSnapshot, styles: ForestStyles, lines: &mut Vec<Line<'static>>) {
    if snapshot.recent_logs.is_empty() {
        return;
    }
    lines.push(Line::from(Span::styled("build logs", styles.summary)));
    for log in snapshot
        .recent_logs
        .iter()
        .rev()
        .take(RENDERED_LOG_LINES)
        .rev()
    {
        lines.push(Line::from(vec![
            Span::styled(format!("{} │ ", log.name), styles.building),
            Span::raw(log.line.clone()),
        ]));
    }
}

fn push_transfers(snapshot: &ForestSnapshot, styles: ForestStyles, lines: &mut Vec<Line<'static>>) {
    if snapshot.transfers.is_empty() {
        return;
    }
    if !lines.is_empty() {
        lines.push(Line::default());
    }
    lines.push(Line::from(Span::styled("transfers", styles.summary)));
    for transfer in snapshot.transfers.iter().take(RENDERED_TRANSFERS) {
        lines.push(Line::from(Span::styled(
            transfer_text(transfer),
            styles.transfer,
        )));
    }
    let hidden = snapshot.transfers.len().saturating_sub(RENDERED_TRANSFERS);
    if hidden > 0 {
        lines.push(Line::from(Span::styled(
            format!("… {hidden} more active transfers"),
            styles.transfer,
        )));
    }
}

fn render_row(row: DisplayRow, styles: ForestStyles) -> Line<'static> {
    let (glyph, style) = match row.node.status {
        DerivationStatus::Unknown => ("?", styles.unknown),
        DerivationStatus::Planned => ("○", styles.planned),
        DerivationStatus::Building => ("▶", styles.building),
        DerivationStatus::Downloading | DerivationStatus::Substituting => ("↓", styles.transfer),
        DerivationStatus::Built => ("✓", styles.built),
        DerivationStatus::Failed => ("✗", styles.failed.add_modifier(Modifier::BOLD)),
    };
    Line::from(vec![
        Span::raw("  ".repeat(row.depth)),
        Span::styled(format!("{glyph} {}", row.node.name), style),
        row.node
            .host
            .filter(|host| !host.is_empty())
            .map(|host| Span::styled(format!(" @ {host}"), style))
            .unwrap_or_else(|| Span::raw("")),
        row.node
            .eta_seconds
            .map(|eta| Span::styled(format!(" · ETA ~{eta}s"), style))
            .unwrap_or_else(|| Span::raw("")),
        row.node
            .last_activity
            .filter(|text| !text.is_empty())
            .map(|text| Span::styled(format!(" — {text}"), style))
            .unwrap_or_else(|| Span::raw("")),
    ])
}

fn transfer_text(transfer: &Transfer) -> String {
    let verb = match transfer.kind {
        ActivityType::FileTransfer => "download",
        ActivityType::CopyPath => "copy",
        ActivityType::Substitute => "substitute",
        _ => "transfer",
    };
    let item = transfer
        .path
        .as_deref()
        .and_then(|path| path.rsplit('/').next())
        .filter(|path| !path.is_empty())
        .unwrap_or("item");
    let mut text = format!("↓ {verb} {item}");
    if let Some(progress) = transfer.progress {
        if transfer.kind == ActivityType::FileTransfer {
            text.push_str(&format!(
                "  {}/{}",
                human_bytes(progress.done),
                human_bytes(progress.expected)
            ));
        } else {
            text.push_str(&format!("  {}/{}", progress.done, progress.expected));
        }
        if let Some(percent) = progress
            .done
            .saturating_mul(100)
            .checked_div(progress.expected)
        {
            text.push_str(&format!(" ({percent}%)"));
        }
    }
    match (&transfer.source, &transfer.destination) {
        (Some(source), Some(destination)) => text.push_str(&format!("  {source} → {destination}")),
        (Some(source), None) => text.push_str(&format!("  from {source}")),
        (None, Some(destination)) => text.push_str(&format!("  to {destination}")),
        (None, None) => {
            if let Some(host) = &transfer.host {
                text.push_str(&format!("  @ {host}"));
            }
        }
    }
    text
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BuildForest;

    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn activity_view_is_bounded_and_puts_logs_above_live_work() {
        let mut forest = BuildForest::new();
        for index in 0..200 {
            forest.feed_line(&format!(
                r#"@nix {{"action":"msg","level":3,"msg":"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-planned-{index:03}.drv"}}"#
            ));
        }
        let active = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-active.drv";
        forest.feed_line(&format!(
            r#"@nix {{"action":"start","id":1,"type":105,"fields":["{active}","builder-a",1,1],"text":"building active"}}"#
        ));
        forest.feed_line(
            r#"@nix {"action":"result","id":1,"type":101,"fields":["compiling crate"]}"#,
        );
        forest.feed_line(
            r#"@nix {"action":"start","id":2,"type":100,"fields":["/nix/store/item","cache-a","builder-a"],"text":"copying item"}"#,
        );
        forest.feed_line(r#"@nix {"action":"result","id":2,"type":105,"fields":[5,10,1,0]}"#);
        let snapshot = forest.snapshot();
        let activity = render_lines(
            &snapshot,
            ForestView::Activity {
                row_budget: DEFAULT_ACTIVITY_ROW_BUDGET,
            },
            ForestStyles::default(),
        );
        let full = render_lines(&snapshot, ForestView::Full, ForestStyles::default());
        let activity_text: Vec<String> = activity.iter().map(text).collect();
        let logs_at = activity_text
            .iter()
            .position(|line| line == "build logs")
            .unwrap();
        let forest_at = activity_text
            .iter()
            .position(|line| line == "activity")
            .unwrap();
        assert!(logs_at < forest_at);
        assert!(activity_text.iter().any(|line| line.contains("active")));
        assert!(activity_text.iter().any(|line| line.contains("5/10 (50%)")));
        assert!(activity_text.iter().any(|line| line.contains("elided")));
        assert!(
            activity.len() < 80,
            "activity view grew to {} lines",
            activity.len()
        );
        assert!(full.len() > 200);
    }
}
