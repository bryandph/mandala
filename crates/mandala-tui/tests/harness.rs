//! Harness spike (task 1.4): the AppState→render seam through TestBackend.
//!
//! Text-grid snapshots capture layout and content, NOT styles — so one
//! style assertion inspects a buffer cell directly, establishing the
//! pattern (design "Risks": states + text grids via insta, style tables as
//! unit-tested pure fns).

use crossterm::event::{Event, KeyCode, KeyEvent};
use futures_util::stream;
use mandala_tui::app::App;
use mandala_tui::render::render;
use mandala_tui::state::{AppState, Status};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};

fn draw(state: &AppState, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal
        .draw(|frame| render(state, frame))
        .expect("render demo state");
    terminal
}

#[test]
fn snapshot_demo_idle() {
    let terminal = draw(&AppState::demo(), 50, 10);
    insta::assert_snapshot!(terminal.backend());
}

#[test]
fn snapshot_working_status_with_cursor_moved() {
    let mut state = AppState::demo();
    state.cursor = 1;
    state.status = Status::Working("evaluating expected".into());
    state.spinner_frame = 2;
    let terminal = draw(&state, 50, 10);
    insta::assert_snapshot!(terminal.backend());
}

#[test]
fn error_status_style_lands_in_the_buffer() {
    let mut state = AppState::demo();
    state.status = Status::Error("deploy failed".into());
    let terminal = draw(&state, 50, 10);
    let buf = terminal.backend().buffer();
    // The status line is the last row; its first glyph carries the error
    // style. Text snapshots can't see this — direct cell inspection is the
    // pattern for style assertions.
    let cell = buf.cell((0, 9)).expect("status cell");
    assert_eq!(cell.symbol(), "✗");
    let style = cell.style();
    assert_eq!(style.fg, Some(Color::Red));
    assert!(style.add_modifier.contains(Modifier::BOLD));
}

fn key(c: char) -> std::io::Result<Event> {
    Ok(Event::Key(KeyEvent::from(KeyCode::Char(c))))
}

/// The REAL loop runs against TestBackend + a synthetic event stream:
/// cursor keys mutate state, `q` quits, and the loop terminates.
#[tokio::test]
async fn loop_moves_cursor_and_quits() {
    let mut terminal = Terminal::new(TestBackend::new(50, 10)).expect("test terminal");
    let mut events = stream::iter(vec![key('j'), key('j'), key('q')]);
    let mut app = App::new(AppState::demo());
    app.run(&mut terminal, &mut events)
        .await
        .expect("loop runs");
    assert_eq!(app.state.cursor, 2);
}

/// An exhausted event stream (terminal gone) also terminates the loop —
/// nothing hangs waiting on a dead terminal.
#[tokio::test]
async fn loop_quits_when_the_stream_closes() {
    let mut terminal = Terminal::new(TestBackend::new(50, 10)).expect("test terminal");
    let mut events = stream::iter(vec![key('j')]);
    let mut app = App::new(AppState::demo());
    app.run(&mut terminal, &mut events)
        .await
        .expect("loop runs");
    assert_eq!(app.state.cursor, 1);
}
