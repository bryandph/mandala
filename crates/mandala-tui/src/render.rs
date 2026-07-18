//! Render fns over `&AppState` — the testable seam.
//!
//! Every fn here takes pure state and a frame/area; none may touch the
//! runtime. Style decisions live in small pure fns (the phase-1 "a status
//! can't ship without a style" construction) so they stay unit-testable
//! without a buffer.
//!
//! Frame shape (the hand-rolled TabbedContent):
//!
//! ```text
//! mandala — fleet                      ← header/title (bold)
//!  members │ groups │ drift            ← tab bar (active reversed)
//!  <table for the active tab>          ← marker col + cursor row reversed
//!  [drift hint line — drift tab only]
//! status line                          ← the sub_title: jobs spinner or rest
//! footer keyhints
//! ```

use mandala_core::drift::DriftStatus;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Row, Table};

use crate::select::{SelectTable, view_offset};
use crate::state::{AppState, Tab};

/// Map ONE rich-style spec from the core's styling vocabulary
/// ([`DriftStatus::style`]) onto a ratatui [`Style`]. This is the single
/// mapping point — no local style table shadows the core's — and it is
/// deliberately partial: an unknown token returns `None`, and the
/// exhaustiveness test below walks [`DriftStatus::ALL`] so a new status (or
/// a new token in the core vocabulary) cannot ship unstyled.
#[must_use]
pub fn rich_style(spec: &str) -> Option<Style> {
    let mut style = Style::new();
    for token in spec.split_whitespace() {
        style = match token {
            "bold" => style.add_modifier(Modifier::BOLD),
            "dim" => style.add_modifier(Modifier::DIM),
            "green" => style.fg(Color::Green),
            "red" => style.fg(Color::Red),
            "yellow" => style.fg(Color::Yellow),
            "magenta" => style.fg(Color::Magenta),
            // The deploy tier's `_STATE_STYLE` vocabulary (screen.rs).
            "cyan" => style.fg(Color::Cyan),
            "blue" => style.fg(Color::Blue),
            _ => return None,
        };
    }
    Some(style)
}

/// The drift-status style, from the core's one vocabulary. The panic is
/// unreachable by the exhaustiveness test over [`DriftStatus::ALL`] below —
/// a status cannot ship unstyled.
#[must_use]
pub fn drift_status_style(status: DriftStatus) -> Style {
    rich_style(status.style())
        .unwrap_or_else(|| panic!("unmapped style token in {:?}", status.style()))
}

/// The selection-marker cell style (`select_table.py`'s `_MARK`).
#[must_use]
pub fn marker_style() -> Style {
    Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
}

/// Status-line style: a sticky error is loud, a running spinner line is
/// live, a resting message is plain.
#[must_use]
pub fn status_line_style(state: &AppState) -> Style {
    if state.status_sticky && !state.any_job_running() {
        Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if state.any_job_running() {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new()
    }
}

/// Footer keyhints with conditional visibility — the `check_action`
/// mechanism: a hint for a surface this session cannot show is noise, so it
/// is absent entirely (the `m` binding appears only under `--debug-mcp`,
/// wired in section 6). Action keys (p/R/D) open the section-5 tier.
#[must_use]
pub fn footer_hints(state: &AppState) -> Vec<(&'static str, &'static str)> {
    let mut hints = vec![
        ("tab", "views"),
        ("space", "select"),
        ("r", "reload"),
        ("S", "refresh drift"),
        ("p", "ping"),
        ("R", "reboot"),
        ("D", "deploy"),
        ("q", "quit"),
    ];
    if state.debug_mcp {
        hints.push(("m", "mcp panel"));
    }
    hints
}

/// Top-level render: the active screen when one is pushed (modals overlay
/// the explorer; task/deploy screens replace it), else the explorer.
pub fn render(state: &AppState, frame: &mut Frame) {
    use crate::screen::{self, ScreenState};
    match &state.screen {
        None => render_explorer(state, frame),
        Some(ScreenState::Confirm(confirm)) => {
            render_explorer(state, frame);
            screen::render_confirm(confirm, frame);
        }
        Some(ScreenState::Reboot(reboot)) => {
            render_explorer(state, frame);
            screen::render_reboot(reboot, frame);
        }
        Some(ScreenState::Task(task)) => screen::render_task(task, frame),
        Some(ScreenState::AttachedLog(attached)) => screen::render_attached(attached, frame),
        Some(ScreenState::Deploy(deploy)) => screen::render_deploy(deploy, frame),
    }
}

/// The explorer view: header, tab bar, active view, status line, footer.
fn render_explorer(state: &AppState, frame: &mut Frame) {
    let [header, tabs, view, status, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(
        Line::from(Span::styled(
            "mandala — fleet",
            Style::new().add_modifier(Modifier::BOLD),
        )),
        header,
    );
    render_tab_bar(state, frame, tabs);
    match state.tab {
        Tab::Members => render_members(state, frame, view),
        Tab::Groups => render_groups(state, frame, view),
        Tab::Drift => render_drift(state, frame, view),
    }
    frame.render_widget(
        Line::from(Span::styled(state.status_line(), status_line_style(state))),
        status,
    );
    render_footer(state, frame, footer);
}

fn render_tab_bar(state: &AppState, frame: &mut Frame, area: Rect) {
    let mut spans = Vec::new();
    for (i, tab) in Tab::ALL.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("│"));
        }
        let style = if tab == state.tab {
            Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::new().add_modifier(Modifier::DIM)
        };
        spans.push(Span::styled(format!(" {} ", tab.title()), style));
    }
    frame.render_widget(Line::from(spans), area);
}

fn render_footer(state: &AppState, frame: &mut Frame, area: Rect) {
    let mut spans = Vec::new();
    for (i, (key, label)) in footer_hints(state).into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                "  ·  ",
                Style::new().add_modifier(Modifier::DIM),
            ));
        }
        spans.push(Span::styled(key, Style::new().add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(
            format!(" {label}"),
            Style::new().add_modifier(Modifier::DIM),
        ));
    }
    frame.render_widget(Line::from(spans), area);
}

/// Render one select-table view: bold header row, marker column from the
/// table's selection, cursor row reversed, viewport window derived from the
/// cursor (no stored offset — render fns take `&AppState`).
fn render_select_table(
    frame: &mut Frame,
    area: Rect,
    table: &SelectTable,
    header: &[&'static str],
    widths: &[Constraint],
    rows: Vec<(String, Vec<Cell<'_>>)>,
) {
    let height = area.height.saturating_sub(1) as usize; // header row
    let offset = view_offset(table.cursor(), height);
    let visible =
        rows.into_iter()
            .enumerate()
            .skip(offset)
            .take(height)
            .map(|(i, (name, cells))| {
                let marker = if table.is_selected(&name) {
                    Cell::from(Span::styled("●", marker_style()))
                } else {
                    Cell::from(" ")
                };
                let mut row = Row::new(std::iter::once(marker).chain(cells));
                if i == table.cursor() {
                    row = row.style(Style::new().add_modifier(Modifier::REVERSED));
                }
                row
            });
    let header_row = Row::new(std::iter::once("").chain(header.iter().copied()))
        .style(Style::new().add_modifier(Modifier::BOLD));
    let widget = Table::new(
        visible,
        std::iter::once(Constraint::Length(1)).chain(widths.iter().copied()),
    )
    .header(header_row)
    .column_spacing(1);
    frame.render_widget(widget, area);
}

fn render_members(state: &AppState, frame: &mut Frame, area: Rect) {
    let rows = state
        .member_rows
        .iter()
        .map(|r| {
            (
                r.name.clone(),
                vec![
                    Cell::from(r.name.clone()),
                    Cell::from(r.platform.clone()),
                    Cell::from(r.arch.clone()),
                    Cell::from(r.category.clone()),
                    Cell::from(r.role.clone()),
                    Cell::from(r.tags.clone()),
                    Cell::from(r.surfaces.clone()),
                ],
            )
        })
        .collect();
    render_select_table(
        frame,
        area,
        &state.members_table,
        &[
            "member", "platform", "arch", "category", "role", "tags", "ads",
        ],
        &[
            Constraint::Length(16),
            Constraint::Length(10),
            Constraint::Length(16),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Fill(1),
            Constraint::Length(3),
        ],
        rows,
    );
}

fn render_groups(state: &AppState, frame: &mut Frame, area: Rect) {
    let rows = state
        .group_rows
        .iter()
        .map(|r| {
            (
                r.name.clone(),
                vec![
                    Cell::from(r.name.clone()),
                    Cell::from(r.n.clone()),
                    Cell::from(r.members.clone()),
                ],
            )
        })
        .collect();
    render_select_table(
        frame,
        area,
        &state.groups_table,
        &["group", "n", "members"],
        &[
            Constraint::Length(16),
            Constraint::Length(4),
            Constraint::Fill(1),
        ],
        rows,
    );
}

fn render_drift(state: &AppState, frame: &mut Frame, area: Rect) {
    let [table_area, hint_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let rows = state
        .drift_rows
        .iter()
        .map(|r| {
            (
                r.name.clone(),
                vec![
                    Cell::from(r.name.clone()),
                    Cell::from(Span::styled(
                        r.status.as_str(),
                        drift_status_style(r.status),
                    )),
                    Cell::from(r.current.clone()),
                    Cell::from(r.expected.clone()),
                    Cell::from(r.booted.clone()),
                    Cell::from(r.captured.clone()),
                ],
            )
        })
        .collect();
    render_select_table(
        frame,
        table_area,
        &state.drift_table,
        &[
            "member", "status", "current", "expected", "booted", "captured",
        ],
        &[
            Constraint::Length(16),
            Constraint::Length(14),
            Constraint::Length(18),
            Constraint::Length(18),
            Constraint::Length(18),
            Constraint::Length(19),
        ],
        rows,
    );
    frame.render_widget(
        Line::from(Span::styled(
            state.drift_hint.clone(),
            Style::new().add_modifier(Modifier::DIM),
        )),
        hint_area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one exhaustiveness gate: every status in the CORE vocabulary maps
    /// (the phase-1 "a status can't ship without a style" construction, now
    /// spanning the crate boundary). A new [`DriftStatus`] extends `ALL`
    /// (compile-enforced there) and lands here; a new token in the core's
    /// style strings fails `rich_style` and this test.
    #[test]
    fn every_drift_status_maps_to_a_ratatui_style() {
        for status in DriftStatus::ALL {
            assert!(
                rich_style(status.style()).is_some(),
                "status {status:?} has an unmapped style spec {:?}",
                status.style()
            );
        }
    }

    #[test]
    fn drift_styles_carry_the_expected_shapes() {
        // Spot-check the mapping semantics (color + modifier composition).
        let drift = drift_status_style(DriftStatus::Drift);
        assert_eq!(drift.fg, Some(Color::Red));
        assert!(drift.add_modifier.contains(Modifier::BOLD));
        let activated = drift_status_style(DriftStatus::Activated);
        assert_eq!(activated.fg, Some(Color::Green));
        assert!(activated.add_modifier.contains(Modifier::DIM));
        let no_snapshot = drift_status_style(DriftStatus::NoSnapshot);
        assert_eq!(no_snapshot.fg, None);
        assert!(no_snapshot.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn unknown_style_token_is_refused_not_defaulted() {
        assert!(rich_style("bold chartreuse").is_none());
    }

    #[test]
    fn mcp_panel_hint_is_conditional_on_debug_mcp() {
        let mut state = AppState::new();
        assert!(!footer_hints(&state).iter().any(|(k, _)| *k == "m"));
        state.debug_mcp = true;
        assert!(footer_hints(&state).iter().any(|(k, _)| *k == "m"));
    }
}
