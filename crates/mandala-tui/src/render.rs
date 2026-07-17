//! Render fns over `&AppState` — the testable seam.
//!
//! Every fn here takes pure state and a frame/area; none may touch the
//! runtime. Style decisions live in small pure fns (the phase-1 "a status
//! can't ship without a style" construction) so they stay unit-testable
//! without a buffer.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Row, Table};

use crate::state::{AppState, RowState, SPINNER_FRAMES, Status};

/// Style for a demo row's lifecycle state — total over [`RowState`]: adding
/// a state without a style is a compile error.
pub fn row_style(state: RowState) -> Style {
    match state {
        RowState::Fresh => Style::new().fg(Color::Green),
        RowState::Drifted => Style::new().fg(Color::Yellow),
        RowState::Unknown => Style::new().add_modifier(Modifier::DIM),
    }
}

/// Style for the status line — total over [`Status`]'s shapes.
pub fn status_style(status: &Status) -> Style {
    match status {
        Status::Idle => Style::new().add_modifier(Modifier::DIM),
        Status::Working(_) => Style::new().fg(Color::Cyan),
        Status::Error(_) => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}

/// Top-level render: the demo table above a one-line status bar.
pub fn render(state: &AppState, frame: &mut Frame) {
    let [main, status] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
    render_table(state, frame, main);
    render_status(state, frame, status);
}

fn render_table(state: &AppState, frame: &mut Frame, area: Rect) {
    let header = Row::new(["name", "role", "st"]).style(Style::new().add_modifier(Modifier::BOLD));
    let rows = state.rows.iter().enumerate().map(|(i, r)| {
        let mut style = row_style(r.state);
        if i == state.cursor {
            style = style.add_modifier(Modifier::REVERSED);
        }
        Row::new([r.name.clone(), r.role.clone(), r.state.glyph().to_string()]).style(style)
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Fill(1),
        ],
    )
    .header(header)
    .block(Block::new().borders(Borders::ALL).title("mandala demo"));
    frame.render_widget(table, area);
}

fn render_status(state: &AppState, frame: &mut Frame, area: Rect) {
    let style = status_style(&state.status);
    let line = match &state.status {
        Status::Idle => Line::from(Span::styled("ready — j/k move · s job · q quit", style)),
        Status::Working(msg) => {
            let glyph = SPINNER_FRAMES[state.spinner_frame % SPINNER_FRAMES.len()];
            Line::from(Span::styled(format!("{glyph} {msg}"), style))
        }
        Status::Error(msg) => Line::from(Span::styled(format!("✗ {msg}"), style)),
    };
    frame.render_widget(line, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_state_has_a_distinct_style() {
        // The style table is pure and total; the distinctness assertion is
        // what keeps a copy-paste from silently merging two states.
        let styles = [
            row_style(RowState::Fresh),
            row_style(RowState::Drifted),
            row_style(RowState::Unknown),
        ];
        assert!(styles[0] != styles[1] && styles[1] != styles[2] && styles[0] != styles[2]);
    }

    #[test]
    fn error_status_is_bold_red() {
        let s = status_style(&Status::Error("boom".into()));
        assert_eq!(s.fg, Some(Color::Red));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }
}
