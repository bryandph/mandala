//! Action-tier flows through the REAL loop on TestBackend — key-driven
//! screen pushes, subprocess streaming, esc semantics, and the
//! after-mutation continuation, with every subprocess an `sh -c` stub
//! (never real ansible). This file is the section-5 analog of the harness
//! real-loop tests, including the mandated end-to-end TaskScreen drill
//! under the real loop (the waker class rides the same select! path).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use chrono::{DateTime, Utc};
use crossterm::event::{Event, KeyCode, KeyEvent};
use futures_util::{Stream, stream};
use mandala_core::inventory::Inventory;
use mandala_tui::app::App;
use mandala_tui::explorer::ExplorerConfig;
use mandala_tui::screen::{REBOOT_UNAVAILABLE, ScreenState};
use mandala_tui::state::{AppState, LoadedInventory};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

// ---- fixtures (the parity aggregate) ----------------------------------------

fn aggregate() -> serde_json::Value {
    json!({
        "schemaVersion": 1,
        "members": {
            "web": {"name": "web", "platform": "metal", "role": "web", "tags": ["edge"]},
            "cache": {"name": "cache", "platform": "metal"},
            "router": {"name": "router", "platform": "opnsense"},
        },
        "groups": {"k3s": ["cache", "web"], "gateway": ["router"]},
        "projections": {"deploy": {"nodes": ["cache", "web"]}},
    })
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-06-12T12:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc)
}

fn filled_state() -> AppState {
    let mut state = AppState::new();
    let req = state.request_load().expect("idle state loads");
    let loaded = LoadedInventory {
        inventory: Inventory::from_value(aggregate()).expect("fixture aggregate is valid"),
        rev: Some("aaaaaaaaaaaaaaaa".to_string()),
        cached_rev: None,
        cached: BTreeMap::new(),
    };
    assert!(
        state
            .on_load_finished(req.generation, Ok(loaded), &BTreeMap::new(), now())
            .is_none()
    );
    state
}

/// A config whose every launch line is a stub — the survey sleeps so a
/// fired after-mutation refresh stays observable at loop exit.
fn stub_cfg() -> ExplorerConfig {
    ExplorerConfig {
        survey_argv: vec!["sh".into(), "-c".into(), "sleep 5".into()],
        ..ExplorerConfig::default()
    }
}

fn key(code: KeyCode) -> io::Result<Event> {
    Ok(Event::Key(KeyEvent::from(code)))
}

/// Run the loop over a finite key list (the stream closing quits the loop
/// with any open screen still inspectable).
async fn run_keys(state: AppState, cfg: ExplorerConfig, events: Vec<io::Result<Event>>) -> App {
    let mut terminal = Terminal::new(TestBackend::new(100, 20)).expect("test terminal");
    let mut events = stream::iter(events);
    let mut app = App::new(state, cfg);
    app.run(&mut terminal, &mut events)
        .await
        .expect("loop runs");
    app
}

/// A stays-open key stream: the sender side drips keys on its own clock so
/// the loop keeps selecting (timers fire, subprocess events land) between
/// them.
fn key_channel() -> (
    tokio::sync::mpsc::UnboundedSender<io::Result<Event>>,
    impl Stream<Item = io::Result<Event>> + Unpin,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let stream = Box::pin(futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|ev| (ev, rx))
    }));
    (tx, stream)
}

async fn run_scripted(
    state: AppState,
    cfg: ExplorerConfig,
    script: Vec<(u64, KeyCode)>,
    close_after: u64,
) -> App {
    let mut terminal = Terminal::new(TestBackend::new(100, 20)).expect("test terminal");
    let (tx, mut events) = key_channel();
    let driver = tokio::spawn(async move {
        for (delay_ms, code) in script {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            let _ = tx.send(key(code));
        }
        tokio::time::sleep(Duration::from_millis(close_after)).await;
        drop(tx); // stream closes → the loop quits
    });
    let mut app = App::new(state, cfg);
    app.run(&mut terminal, &mut events)
        .await
        .expect("loop runs");
    driver.await.expect("driver task");
    app
}

// ---- ping: TaskScreen end-to-end under the real loop ------------------------

fn stub_ping_ok(_target: &str) -> Vec<String> {
    vec!["sh".into(), "-c".into(), "echo ping-ok".into()]
}

#[tokio::test]
async fn ping_streams_lines_and_exit_through_the_real_loop() {
    let cfg = ExplorerConfig {
        ping_argv: stub_ping_ok,
        ..stub_cfg()
    };
    // p pushes the task screen; the stream stays open while the subprocess
    // runs so its lines and settle flow through the loop; then it closes
    // with the screen still up for inspection.
    let app = run_scripted(filled_state(), cfg, vec![(0, KeyCode::Char('p'))], 1500).await;
    let Some(ScreenState::Task(task)) = &app.state.screen else {
        panic!("task screen not up: {:?}", app.state.screen);
    };
    assert_eq!(task.title, "ping cache"); // cursor row = cache
    assert!(task.launched);
    assert!(!task.after_mutation); // ping refreshes nothing (Python parity)
    let lines: Vec<&str> = task.lines.iter().map(String::as_str).collect();
    assert!(
        lines[0].starts_with("$ sh -c echo ping-ok  (cwd="),
        "{lines:?}"
    );
    assert!(lines.contains(&"ping-ok"), "{lines:?}");
    assert_eq!(*lines.last().unwrap(), "— exit 0");
    assert_eq!(task.rc, Some(0));
}

#[tokio::test]
async fn ping_targets_the_selection_comma_joined() {
    let mut state = filled_state();
    state.members_table.toggle(); // cache
    state.members_table.skip(2); // cursor to web
    state.members_table.toggle(); // + web
    let cfg = ExplorerConfig {
        ping_argv: stub_ping_ok,
        ..stub_cfg()
    };
    let app = run_keys(state, cfg, vec![key(KeyCode::Char('p'))]).await;
    let Some(ScreenState::Task(task)) = &app.state.screen else {
        panic!("task screen not up");
    };
    assert_eq!(task.title, "ping cache,web");
}

fn stub_launch_failure(_target: &str) -> Vec<String> {
    vec!["/nonexistent/mandala-task-stub".into()]
}

#[tokio::test]
async fn task_launch_failure_is_surfaced_in_pane() {
    let cfg = ExplorerConfig {
        ping_argv: stub_launch_failure,
        ..stub_cfg()
    };
    let app = run_keys(filled_state(), cfg, vec![key(KeyCode::Char('p'))]).await;
    let Some(ScreenState::Task(task)) = &app.state.screen else {
        panic!("task screen not up");
    };
    assert!(!task.launched);
    assert_eq!(task.rc, None);
    let lines: Vec<&str> = task.lines.iter().map(String::as_str).collect();
    assert!(lines[0].starts_with("$ /nonexistent/mandala-task-stub"));
    assert!(lines[1].starts_with("failed to launch: "), "{lines:?}");
}

fn stub_sleep(_target: &str) -> Vec<String> {
    vec!["sh".into(), "-c".into(), "sleep 30".into()]
}

#[tokio::test]
async fn esc_on_a_running_task_dismisses_with_rc_none() {
    let cfg = ExplorerConfig {
        ping_argv: stub_sleep,
        ..stub_cfg()
    };
    // p → running stub; esc terminates-then-dismisses; rc is still None
    // (the subprocess hasn't been reaped) → operator cancel, no refresh.
    let app = run_scripted(
        filled_state(),
        cfg,
        vec![(0, KeyCode::Char('p')), (400, KeyCode::Esc)],
        200,
    )
    .await;
    assert!(app.state.screen.is_none());
    assert!(!app.state.surveying, "cancel must not refresh drift");
    assert!(!app.state.busy);
}

// ---- reboot: pre-check, modal flow, after-mutation --------------------------

fn reboot_unavailable(_t: &str, _s: &str, _d: bool) -> Option<Vec<String>> {
    None
}

#[tokio::test]
async fn reboot_pre_check_surfaces_the_unavailable_message() {
    let cfg = ExplorerConfig {
        reboot_argv: reboot_unavailable,
        ..stub_cfg()
    };
    let app = run_keys(filled_state(), cfg, vec![key(KeyCode::Char('R'))]).await;
    assert!(app.state.screen.is_none(), "no modal without the wrapper");
    assert_eq!(
        app.state.status,
        "no ans-reboot wrapper or playbooks/reboot.yaml — reboot task unavailable"
    );
    assert_eq!(app.state.status, REBOOT_UNAVAILABLE);
}

thread_local! {
    static REBOOT_CALLS: RefCell<Vec<(String, String, bool)>> = const { RefCell::new(Vec::new()) };
}

fn reboot_capture(target: &str, serial: &str, drain: bool) -> Option<Vec<String>> {
    REBOOT_CALLS.with(|calls| {
        calls
            .borrow_mut()
            .push((target.to_string(), serial.to_string(), drain));
    });
    Some(vec!["sh".into(), "-c".into(), "echo reboot-ran".into()])
}

#[tokio::test]
async fn reboot_modal_options_ride_into_the_shared_argv() {
    REBOOT_CALLS.with(|calls| calls.borrow_mut().clear());
    let cfg = ExplorerConfig {
        reboot_argv: reboot_capture,
        ..stub_cfg()
    };
    // R → modal (pre-check probes with serial=1, drain=true), 3 → all-at-
    // once, d → skip drain, y → the chosen options reach reboot_argv.
    let app = run_keys(
        filled_state(),
        cfg,
        vec![
            key(KeyCode::Char('R')),
            key(KeyCode::Char('3')),
            key(KeyCode::Char('d')),
            key(KeyCode::Char('y')),
        ],
    )
    .await;
    let calls = REBOOT_CALLS.with(|calls| calls.borrow().clone());
    assert_eq!(
        calls,
        vec![
            ("cache".to_string(), "1".to_string(), true), // availability pre-check
            ("cache".to_string(), "100%".to_string(), false), // the chosen options
        ]
    );
    let Some(ScreenState::Task(task)) = &app.state.screen else {
        panic!("reboot task screen not up: {:?}", app.state.screen);
    };
    assert_eq!(task.title, "reboot cache");
    assert!(task.after_mutation); // a completed reboot refreshes drift
    assert!(task.lines[0].starts_with("$ sh -c echo reboot-ran  (cwd="));
}

#[tokio::test]
async fn reboot_modal_esc_cancels_without_running() {
    REBOOT_CALLS.with(|calls| calls.borrow_mut().clear());
    let cfg = ExplorerConfig {
        reboot_argv: reboot_capture,
        ..stub_cfg()
    };
    let app = run_keys(
        filled_state(),
        cfg,
        vec![key(KeyCode::Char('R')), key(KeyCode::Esc)],
    )
    .await;
    assert!(app.state.screen.is_none());
    // Only the availability pre-check probed; nothing launched.
    let calls = REBOOT_CALLS.with(|calls| calls.borrow().clone());
    assert_eq!(calls.len(), 1);
}

fn reboot_quick(_t: &str, _s: &str, _d: bool) -> Option<Vec<String>> {
    Some(vec!["sh".into(), "-c".into(), "echo done".into()])
}

#[tokio::test]
async fn completed_reboot_dismissal_fires_the_drift_refresh() {
    let cfg = ExplorerConfig {
        reboot_argv: reboot_quick,
        ..stub_cfg()
    };
    // R → y (defaults) → the stub exits 0 → esc dismisses with rc Some(0)
    // → the after-mutation rule fires the concurrent eval+survey refresh
    // (the survey stub sleeps, so the flag is still up at loop exit).
    let app = run_scripted(
        filled_state(),
        cfg,
        vec![
            (0, KeyCode::Char('R')),
            (50, KeyCode::Char('y')),
            (1200, KeyCode::Esc),
        ],
        200,
    )
    .await;
    assert!(app.state.screen.is_none());
    assert!(
        app.state.surveying,
        "completed mutation must auto-refresh drift"
    );
}

// ---- deploy: confirm gate ---------------------------------------------------

#[tokio::test]
async fn deploy_confirm_gate_message_and_cancel() {
    let app = run_keys(filled_state(), stub_cfg(), vec![key(KeyCode::Char('D'))]).await;
    let Some(ScreenState::Confirm(confirm)) = &app.state.screen else {
        panic!("confirm modal not up: {:?}", app.state.screen);
    };
    assert_eq!(
        confirm.message,
        "Deploy 'cache'?\n(eval-once batch build, then deploy-rs per host with magic rollback)"
    );
    // n cancels; nothing launches, no refresh.
    let app = run_keys(
        filled_state(),
        stub_cfg(),
        vec![key(KeyCode::Char('D')), key(KeyCode::Char('n'))],
    )
    .await;
    assert!(app.state.screen.is_none());
    assert!(!app.state.surveying && !app.state.busy);
}

#[tokio::test]
async fn actions_without_a_target_are_noops() {
    // An empty explorer has no target: p/R/D push nothing.
    let app = run_keys(
        AppState::new(),
        stub_cfg(),
        vec![
            key(KeyCode::Char('p')),
            key(KeyCode::Char('R')),
            key(KeyCode::Char('D')),
        ],
    )
    .await;
    assert!(app.state.screen.is_none());
}
