//! The AppState→render seam through TestBackend, and the REAL loop driven
//! by synthetic key streams.
//!
//! Text-grid snapshots capture layout and content, NOT styles — so style
//! assertions inspect buffer cells directly (design "Risks": states + text
//! grids via insta, style tables as unit-tested pure fns).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use futures_util::stream;
use mandala_core::drift::Snapshot;
use mandala_core::inventory::Inventory;
use mandala_tui::app::App;
use mandala_tui::explorer::ExplorerConfig;
use mandala_tui::hit::Target;
use mandala_tui::keymap::Action;
use mandala_tui::render::{render, render_with_theme};
use mandala_tui::screen::{ScreenState, TaskState};
use mandala_tui::state::{AppState, LoadedInventory, Tab};
use mandala_tui::theme::Theme;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use serde_json::json;

// ---- fixtures (the parity aggregate: web/cache/router, k3s/gateway) ---------

fn aggregate() -> serde_json::Value {
    json!({
        "schemaVersion": 1,
        "members": {
            "web": {
                "name": "web",
                "platform": "metal",
                "architecture": "x86_64-linux",
                "category": "server",
                "role": "web",
                "tags": ["edge"],
                "deployment": {"ansible": {"enable": true}, "deployRs": {"enable": true}},
            },
            "cache": {"name": "cache", "platform": "metal", "architecture": "x86_64-linux"},
            "router": {"name": "router", "platform": "opnsense"},
        },
        "groups": {"k3s": ["cache", "web"], "gateway": ["router"]},
        "projections": {"deploy": {"nodes": ["cache", "web"]}},
    })
}

fn loaded(rev: Option<&str>, cached_rev: Option<&str>) -> LoadedInventory {
    LoadedInventory {
        inventory: Inventory::from_value(aggregate()).expect("fixture aggregate is valid"),
        rev: rev.map(str::to_string),
        cached_rev: cached_rev.map(str::to_string),
        cached: BTreeMap::new(),
    }
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-06-12T12:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc)
}

fn snap(current: &str, booted: &str) -> Snapshot {
    serde_json::from_value(json!({
        "current": current,
        "booted": booted,
        "captured_at": "2026-06-12T11:59:00+00:00",
    }))
    .expect("snapshot fixture parses")
}

fn snapshots() -> BTreeMap<String, Snapshot> {
    BTreeMap::from([
        (
            "cache".to_string(),
            snap(
                "/nix/store/curcache-toplevel",
                "/nix/store/curcache-toplevel",
            ),
        ),
        (
            "web".to_string(),
            snap(
                "/nix/store/curweb00-toplevel",
                "/nix/store/curweb00-toplevel",
            ),
        ),
    ])
}

/// A filled explorer state: the fixture fleet painted, no expected cache.
fn filled_state() -> AppState {
    let mut state = AppState::new();
    let req = state.request_load().expect("idle state loads");
    assert!(
        state
            .on_load_finished(
                req.generation,
                Ok(loaded(Some("aaaaaaaaaaaaaaaa"), None)),
                &snapshots(),
                now()
            )
            .is_none()
    );
    state
}

/// A filled state where the expected eval landed: `cache` drifts (moved
/// contract), `web` is in sync.
fn drift_state() -> AppState {
    let mut state = AppState::new();
    let _ = state.request_load();
    let _ = state.on_load_finished(
        0,
        Ok(loaded(Some("aaaaaaaaaaaaaaaa"), None)),
        &snapshots(),
        now(),
    );
    assert!(state.request_eval_expected());
    let expected = BTreeMap::from([
        (
            "cache".to_string(),
            "/nix/store/expcache-toplevel".to_string(),
        ),
        (
            "web".to_string(),
            "/nix/store/curweb00-toplevel".to_string(),
        ),
    ]);
    let _ = state.on_drift_eval_finished(
        Ok((Some("aaaaaaaaaaaaaaaa".to_string()), expected)),
        &snapshots(),
        now(),
    );
    state.tab = Tab::Drift;
    state
}

fn draw(state: &AppState, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal
        .draw(|frame| render(state, frame))
        .expect("render explorer state");
    terminal
}

// ---- snapshots --------------------------------------------------------------

#[test]
fn snapshot_members_view() {
    let mut state = filled_state();
    // One toggled row so the marker column and the resting status both show.
    state.members_table.toggle();
    let terminal = draw(&state, 100, 12);
    insta::assert_snapshot!(terminal.backend());
}

#[test]
fn snapshot_groups_view() {
    let mut state = filled_state();
    state.tab = Tab::Groups;
    let terminal = draw(&state, 80, 10);
    insta::assert_snapshot!(terminal.backend());
}

#[test]
fn snapshot_drift_view_with_expected_caption() {
    let terminal = draw(&drift_state(), 120, 12);
    insta::assert_snapshot!(terminal.backend());
}

#[test]
fn snapshot_concurrent_jobs_spinner_line() {
    let mut state = filled_state();
    let (eval, survey) = state.refresh_drift();
    assert!(eval && survey);
    state.on_survey_progress(2);
    let terminal = draw(&state, 100, 12);
    insta::assert_snapshot!(terminal.backend());
}

#[test]
fn render_builds_hit_targets_for_tabs_and_visible_rows() {
    let state = filled_state();
    let mut terminal = Terminal::new(TestBackend::new(100, 12)).expect("test terminal");
    let mut hits = None;
    terminal
        .draw(|frame| hits = Some(render_with_theme(&state, frame, &Theme::default())))
        .expect("render explorer hit map");
    let hits = hits.expect("hit map returned");

    assert_eq!(hits.hit(11, 1), Some(&Target::Action(Action::TabTwo)));
    assert_eq!(
        hits.hit(4, 4),
        Some(&Target::ExplorerRow {
            tab: Tab::Members,
            index: 0,
        })
    );
}

// ---- buffer-cell style assertions (snapshots can't see styles) --------------

#[test]
fn drift_status_cell_carries_the_core_style() {
    let state = drift_state();
    let terminal = draw(&state, 120, 12);
    let buf = terminal.backend().buffer();
    // The focused pane border shifts the table one cell right/down. The
    // first drift status begins at x=20, y=4.
    let cell = buf.cell((20, 4)).expect("status cell");
    assert_eq!(cell.symbol(), "d"); // "drift"
    assert_eq!(cell.style().fg, Some(Color::Red));
    assert!(cell.style().add_modifier.contains(Modifier::BOLD));
    // Second row (web) is in-sync: green, not bold.
    let cell = buf.cell((20, 5)).expect("status cell");
    assert_eq!(cell.symbol(), "i"); // "in-sync"
    assert_eq!(cell.style().fg, Some(Color::Green));
    assert!(!cell.style().add_modifier.contains(Modifier::BOLD));
}

#[test]
fn selection_marker_cell_is_bold_cyan() {
    let mut state = filled_state();
    state.members_table.toggle(); // cursor row 0 = "cache"
    let terminal = draw(&state, 100, 12);
    let buf = terminal.backend().buffer();
    // The focused pane border shifts the first member row to (1, 4). The
    // cursor row is REVERSED on top of the marker style.
    let cell = buf.cell((1, 4)).expect("marker cell");
    assert_eq!(cell.symbol(), "●");
    assert_eq!(cell.style().fg, Some(Color::Cyan));
    assert!(cell.style().add_modifier.contains(Modifier::BOLD));
}

#[test]
fn explorer_focus_is_distinct_from_the_inactive_activity_pane() {
    let mut state = filled_state();
    state.debug_mcp = true;
    state.mcp_panel = true;
    let terminal = draw(&state, 100, 12);
    let buf = terminal.backend().buffer();

    let focused = buf.cell((0, 2)).expect("focused explorer border");
    assert_eq!(focused.symbol(), "┌");
    assert_eq!(focused.style().fg, Some(Color::LightCyan));
    assert!(focused.style().add_modifier.contains(Modifier::BOLD));

    let inactive = buf.cell((48, 0)).expect("inactive activity border");
    assert_eq!(inactive.symbol(), "┌");
    assert_eq!(inactive.style().fg, Some(Color::DarkGray));
    assert!(!inactive.style().add_modifier.contains(Modifier::BOLD));
}

// ---- the REAL loop under TestBackend ----------------------------------------

fn key(code: KeyCode) -> std::io::Result<Event> {
    Ok(Event::Key(KeyEvent::from(code)))
}

fn key_mod(code: KeyCode, modifiers: KeyModifiers) -> std::io::Result<Event> {
    Ok(Event::Key(KeyEvent::new(code, modifiers)))
}

fn click(column: u16, row: u16) -> std::io::Result<Event> {
    Ok(Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }))
}

fn wheel(kind: MouseEventKind, column: u16, row: u16) -> std::io::Result<Event> {
    Ok(Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }))
}

async fn run_keys(state: AppState, events: Vec<std::io::Result<Event>>) -> App {
    let mut terminal = Terminal::new(TestBackend::new(100, 12)).expect("test terminal");
    let mut events = stream::iter(events);
    let mut app = App::new(state, ExplorerConfig::default());
    app.run(&mut terminal, &mut events)
        .await
        .expect("loop runs");
    app
}

/// Selection through the real loop: space toggles, shift+down extends (and
/// moves the cursor), and the target is the comma-joined selection.
#[tokio::test]
async fn loop_selection_keys_drive_the_active_table() {
    let app = run_keys(
        filled_state(),
        vec![
            key(KeyCode::Char(' ')),                     // toggle "cache"
            key_mod(KeyCode::Down, KeyModifiers::SHIFT), // extend to "router"
            key(KeyCode::Char('q')),
        ],
    )
    .await;
    assert_eq!(app.state.members_table.cursor(), 1);
    assert_eq!(
        app.state.members_table.selected_names(),
        ["cache", "router"]
    );
    assert_eq!(app.state.target().as_deref(), Some("cache,router"));
}

/// Tab switching: `tab` cycles, digits jump, `backtab` cycles backwards —
/// and the target follows the ACTIVE tab's table.
#[tokio::test]
async fn loop_tab_keys_switch_views() {
    let app = run_keys(
        filled_state(),
        vec![
            key(KeyCode::Tab), // members → groups
            key(KeyCode::Char('q')),
        ],
    )
    .await;
    assert_eq!(app.state.tab, Tab::Groups);
    // Groups table cursor sits on "gateway" (sorted first).
    assert_eq!(app.state.target().as_deref(), Some("gateway"));

    let app = run_keys(
        filled_state(),
        vec![
            key(KeyCode::Char('3')),
            key(KeyCode::BackTab), // drift → groups
            key(KeyCode::Char('q')),
        ],
    )
    .await;
    assert_eq!(app.state.tab, Tab::Groups);
}

#[tokio::test]
async fn loop_hjkl_and_vim_motions_drive_the_active_view() {
    let app = run_keys(
        filled_state(),
        vec![
            key(KeyCode::Right),
            key(KeyCode::Char('h')),
            key(KeyCode::Char('G')),
            key(KeyCode::Char('g')),
            key_mod(KeyCode::Char('d'), KeyModifiers::CONTROL),
            key(KeyCode::Char('q')),
        ],
    )
    .await;
    assert_eq!(app.state.tab, Tab::Members);
    assert_eq!(app.state.members_table.cursor(), 2);
}

#[tokio::test]
async fn loop_mouse_clicks_select_rows_and_activate_tabs() {
    let app = run_keys(filled_state(), vec![click(4, 4), click(11, 1)]).await;
    assert_eq!(app.state.members_table.selected_names(), ["cache"]);
    assert_eq!(app.state.tab, Tab::Groups);
}

#[tokio::test]
async fn loop_mouse_wheel_scrolls_the_hovered_text_pane() {
    let mut task = TaskState::new("ping web", 1, false);
    for i in 0..20 {
        task.push_line(format!("line {i:02}"));
    }
    let mut state = AppState::new();
    state.screen = Some(ScreenState::Task(task));

    let app = run_keys(state, vec![wheel(MouseEventKind::ScrollUp, 4, 2)]).await;
    let Some(ScreenState::Task(task)) = app.state.screen else {
        panic!("task screen missing");
    };
    assert!(!task.scroll.follow());
    assert_eq!(task.scroll.offset_from_tail(), 3);
}

#[tokio::test]
async fn loop_task_scrollback_unpins_and_bottom_repins() {
    let task_state = || {
        let mut task = TaskState::new("ping web", 1, false);
        for i in 0..20 {
            task.push_line(format!("line {i:02}"));
        }
        let mut state = AppState::new();
        state.screen = Some(ScreenState::Task(task));
        state
    };

    let app = run_keys(task_state(), vec![key(KeyCode::PageUp)]).await;
    let Some(ScreenState::Task(task)) = app.state.screen else {
        panic!("task screen missing");
    };
    assert!(!task.scroll.follow());
    assert!(task.scroll.offset_from_tail() > 0);

    let app = run_keys(
        task_state(),
        vec![key(KeyCode::PageUp), key(KeyCode::Char('G'))],
    )
    .await;
    let Some(ScreenState::Task(task)) = app.state.screen else {
        panic!("task screen missing");
    };
    assert!(task.scroll.follow());
}

/// esc clears the selection (it does NOT quit the explorer); ctrl+down
/// skips without selecting.
#[tokio::test]
async fn loop_esc_clears_selection_and_ctrl_moves_skip() {
    let app = run_keys(
        filled_state(),
        vec![
            key(KeyCode::Char(' ')),
            key_mod(KeyCode::Down, KeyModifiers::CONTROL),
            key(KeyCode::Esc),
            key(KeyCode::Char('q')),
        ],
    )
    .await;
    assert!(app.state.members_table.selected_names().is_empty());
    assert_eq!(app.state.members_table.cursor(), 1);
    // Cursor fallback: esc cleared the selection, so target = cursor row.
    assert_eq!(app.state.target().as_deref(), Some("router"));
}

/// An exhausted event stream (terminal gone) also terminates the loop —
/// nothing hangs waiting on a dead terminal.
#[tokio::test]
async fn loop_quits_when_the_stream_closes() {
    let app = run_keys(filled_state(), vec![key(KeyCode::Down)]).await;
    assert_eq!(app.state.members_table.cursor(), 1);
}
