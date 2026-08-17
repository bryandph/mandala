//! Deploy-runner flows through the REAL loop: owned launch (confirm-gated),
//! owned-vs-attached esc semantics, the finish→summary transition, the
//! after-mutation refresh, and the standalone exit-code mapping. Every
//! subprocess is an `sh -c` stub (the `DeployRun::program` seam — never
//! real ansible/nix), and the run registry is a private tmp base via
//! `MANDALA_FLEET_STATE` (set once, before any test reads it).

use std::collections::BTreeMap;
use std::io;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use futures_util::Stream;
use mandala_core::inventory::Inventory;
use mandala_core::registry::{self, Meta};
use mandala_core::runner::{DeployRun, HostState};
use mandala_tui::app::App;
use mandala_tui::explorer::ExplorerConfig;
use mandala_tui::screen::{DeployTab, ScreenState};
use mandala_tui::state::{AppState, LoadedInventory};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::{Value, json};

// ---- process-wide registry base ---------------------------------------------

fn registry_env() -> &'static PathBuf {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    BASE.get_or_init(|| {
        // This suite exercises the real durable-supervisor boundary. The
        // workspace build places that binary beside the test profile; use the
        // same sibling lookup as production instead of recursively invoking
        // Cargo (which also breaks target/profile selection in Nix checks).
        let dir = std::env::temp_dir().join(format!("mandala-deployflow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: set once, under the OnceLock, before any concurrent read
        // in this process (every test calls registry_env() first).
        unsafe {
            std::env::set_var("MANDALA_FLEET_STATE", &dir);
            std::env::set_var("MANDALA_FLEET_RUN_KEEP", "500");
            std::env::remove_var("MANDALA_DEPLOY_SUPERVISOR_BIN");
        }
        dir
    })
}

// ---- fixtures ---------------------------------------------------------------

fn aggregate() -> serde_json::Value {
    json!({
        "schemaVersion": 1,
        "members": {
            "web": {"name": "web", "platform": "metal"},
            "cache": {"name": "cache", "platform": "metal"},
        },
        "groups": {"k3s": ["cache", "web"]},
        "projections": {"deploy": {"nodes": ["cache", "web"]}},
    })
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
    let now: DateTime<Utc> = DateTime::parse_from_rfc3339("2026-06-12T12:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc);
    assert!(
        state
            .on_load_finished(req.generation, Ok(loaded), &BTreeMap::new(), now)
            .is_none()
    );
    state
}

/// Survey stubbed to sleep so a fired after-mutation refresh stays
/// observable; the deploy argv comes per-test via `deploy_program`.
fn stub_cfg(deploy_program: &[&str]) -> ExplorerConfig {
    assert_eq!(&deploy_program[..2], ["sh", "-c"]);
    let body = deploy_program[2];
    // Emulate the native engine boundary: the frontend/supervisor passes no
    // events directory. The engine stub allocates its own registry run,
    // publishes that id, then writes events there.
    let native = format!(
        r#"test -z "$MANDALA_FLEET_EVENTS" || exit 91
rid="20260721T120000_000000-$$"
events="$MANDALA_FLEET_STATE/runs/$rid"
mkdir -p "$events"
printf '{{"run_id":"%s","limit":"web","pid":%s,"started_at":0}}\n' "$rid" "$$" > "$events/meta.json"
printf '%s\n' "$rid"
export MANDALA_NATIVE_TEST_EVENTS="$events"
{body}"#
    );
    ExplorerConfig {
        survey_argv: vec!["sh".into(), "-c".into(), "sleep 5".into()],
        deploy_program: Some(vec!["sh".into(), "-c".into(), native]),
        ..ExplorerConfig::default()
    }
}

fn key(code: KeyCode) -> io::Result<Event> {
    Ok(Event::Key(KeyEvent::from(code)))
}

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

/// Drive an app (with any pre-pushed screen) through timed keys, closing
/// the stream `close_after` ms after the last key.
async fn drive(mut app: App, script: Vec<(u64, KeyCode)>, close_after: u64) -> App {
    let mut terminal = Terminal::new(TestBackend::new(100, 20)).expect("test terminal");
    let (tx, mut events) = key_channel();
    let driver = tokio::spawn(async move {
        for (delay_ms, code) in script {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            let _ = tx.send(key(code));
        }
        tokio::time::sleep(Duration::from_millis(close_after)).await;
        drop(tx);
    });
    app.run(&mut terminal, &mut events)
        .await
        .expect("loop runs");
    driver.await.expect("driver task");
    app
}

fn write_events(path: &Path, events: &[Value]) {
    let mut fh = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    for e in events {
        let mut obj = serde_json::Map::new();
        obj.insert("v".into(), Value::from(1));
        obj.insert("ts".into(), Value::from(0.0));
        for (k, v) in e.as_object().unwrap() {
            obj.insert(k.clone(), v.clone());
        }
        writeln!(fh, "{}", Value::Object(obj)).unwrap();
    }
}

fn milestones(host: &str, names: &[&str]) -> Vec<Value> {
    names
        .iter()
        .map(|n| json!({"host":host,"plugin":"deploy","event":"milestone","milestone":n}))
        .collect()
}

// ---- owned mode (confirm-gated launch from the explorer) --------------------

/// The confirm-gated launch streams the run's own event files back into the
/// screen: the native stub writes build + milestone JSONL into its own
/// registry run and the 250ms poll surfaces them as nom input + a host tab.
#[tokio::test]
async fn owned_deploy_launches_and_renders_events_live() {
    registry_env();
    let cfg = stub_cfg(&[
        "sh",
        "-c",
        r#"printf '%s\n' '{"v":2,"ts":0,"host":"build","plugin":"build","event":"nixlog","line":"@nix {\"action\":\"start\",\"type\":105}"}' > "$MANDALA_NATIVE_TEST_EVENTS/build.jsonl"; printf '{"v":1,"ts":0,"host":"web","plugin":"deploy","event":"milestone","milestone":"activate"}\n' > "$MANDALA_NATIVE_TEST_EVENTS/web.jsonl"; sleep 3"#,
    ]);
    let app = drive(
        App::new(filled_state(), cfg),
        vec![(0, KeyCode::Char('D')), (100, KeyCode::Char('y'))],
        1200,
    )
    .await;
    let Some(ScreenState::Deploy(view)) = &app.state.screen else {
        panic!("deploy screen not up: {:?}", app.state.screen);
    };
    assert_eq!(view.limit, "cache"); // target = cursor row
    assert!(!view.attached && !view.standalone && view.after_mutation);
    assert!(!view.finished);
    assert_eq!(view.sub_title(), "-l cache");
    assert_eq!(view.active, DeployTab::Build);
    // The stub's event landed as a host tab through the live poll.
    assert_eq!(view.hosts.len(), 1, "host tab missing: {view:?}");
    assert_eq!(view.hosts[0].name, "web");
    assert_eq!(view.hosts[0].state, HostState::Activating);
    assert_eq!(
        app.deploy_nixlog_lines_seen(),
        1,
        "build record emitted before attach must reach nom"
    );
    // The engine-output mirror leads with the argv header.
    assert!(view.playbook_lines[0].starts_with("$ sh -c "));
}

#[tokio::test]
async fn owned_esc_detaches_without_refresh_or_termination() {
    registry_env();
    let cfg = stub_cfg(&["sh", "-c", "sleep 2"]);
    // esc DETACHES the running deploy (never terminates — runs are
    // engine-owned) and dismisses with rc None; no immediate drift refresh
    // (that rides the settlement watch once the run completes).
    let app = drive(
        App::new(filled_state(), cfg),
        vec![
            (0, KeyCode::Char('D')),
            (100, KeyCode::Char('y')),
            (600, KeyCode::Esc),
        ],
        200,
    )
    .await;
    assert!(app.state.screen.is_none());
    assert!(
        !app.state.surveying,
        "detach must not refresh drift eagerly"
    );
}

#[tokio::test]
async fn deploy_ctrl_k_arms_then_y_terminates() {
    registry_env();
    let cfg = stub_cfg(&["sh", "-c", "sleep 30"]);
    // ctrl-k arms the terminate confirmation, y signals the run (SIGTERM),
    // the run settles killed, and esc then dismisses a finished screen.
    let mut terminal = Terminal::new(TestBackend::new(100, 20)).expect("test terminal");
    let (tx, mut events) = key_channel();
    let driver = tokio::spawn(async move {
        let _ = tx.send(key(KeyCode::Char('D')));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = tx.send(key(KeyCode::Char('y'))); // confirm launch
        tokio::time::sleep(Duration::from_millis(600)).await;
        let _ = tx.send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('k'),
            KeyModifiers::CONTROL,
        ))));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = tx.send(key(KeyCode::Char('y'))); // confirm terminate
        // Give SIGTERM time to land and the poll to reap it.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        drop(tx);
    });
    let mut app = App::new(filled_state(), cfg);
    app.run(&mut terminal, &mut events)
        .await
        .expect("loop runs");
    driver.await.expect("driver task");
    // The screen stays up (no esc sent): the gated terminate must have
    // killed the sleep-30 stub — finished, non-zero (signal) rc, disarmed.
    let Some(ScreenState::Deploy(view)) = &app.state.screen else {
        panic!("deploy screen expected, got {:?}", app.state.screen);
    };
    assert!(!view.kill_armed, "y must disarm the gate");
    assert!(view.finished, "terminate must settle the run");
    assert!(
        view.returncode.is_some_and(|rc| rc != 0),
        "a terminated run reports a non-zero rc, got {:?}",
        view.returncode
    );
}

#[tokio::test]
async fn dropping_frontend_leaves_supervised_native_child_alive() {
    let base = registry_env();
    let marker = base.join(format!(
        "frontend-drop-survived-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let body = format!("sleep 0.4; printf survived > '{}'", marker.display());
    let cfg = stub_cfg(&["sh", "-c", &body]);
    let mut run = DeployRun::new("web");
    run.program = cfg.deploy_program;
    let mut app = App::new(AppState::new(), ExplorerConfig::default());
    assert!(app.start_deploy(run, false, false, false, (100, 20)).await);
    drop(app);
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(marker.is_file(), "supervised engine died with its frontend");
}

#[tokio::test]
async fn owned_finish_materializes_summary_with_recap() {
    registry_env();
    // The marker is assembled at runtime so the argv HEADER line (mirrored
    // first) can't satisfy the recap search — only the real output can.
    let cfg = stub_cfg(&[
        "sh",
        "-c",
        r#"R=RECAP; echo "PLAY $R *********"; echo "web : ok=3 failed=0"; exit 0"#,
    ]);
    let app = drive(
        App::new(filled_state(), cfg),
        vec![(0, KeyCode::Char('D')), (100, KeyCode::Char('y'))],
        1400,
    )
    .await;
    let Some(ScreenState::Deploy(view)) = &app.state.screen else {
        panic!("deploy screen not up");
    };
    assert!(view.finished);
    assert_eq!(view.returncode, Some(0));
    assert_eq!(view.sub_title(), "-l cache — exit 0");
    // The summary tab materialized and took focus.
    assert_eq!(view.active, DeployTab::Summary);
    let summary = view.summary.as_ref().expect("summary");
    assert_eq!(summary.head, "deploy succeeded");
    assert_eq!(summary.recap[0], "PLAY RECAP *********");
    assert_eq!(summary.recap[1], "web : ok=3 failed=0");
    assert_eq!(summary.recap.len(), 2);
}

#[tokio::test]
async fn owned_zero_exit_with_rollback_materializes_failed_summary() {
    registry_env();
    let cfg = stub_cfg(&[
        "sh",
        "-c",
        r#"printf '%s\n' '{"v":1,"ts":0,"host":"web","plugin":"deploy","event":"milestone","milestone":"rollback"}' > "$MANDALA_NATIVE_TEST_EVENTS/web.jsonl"; exit 0"#,
    ]);
    let app = drive(
        App::new(filled_state(), cfg),
        vec![(0, KeyCode::Char('D')), (100, KeyCode::Char('y'))],
        1400,
    )
    .await;
    let Some(ScreenState::Deploy(view)) = &app.state.screen else {
        panic!("deploy screen not up");
    };
    assert!(view.finished);
    assert_eq!(view.returncode, Some(1));
    assert_eq!(view.hosts[0].state, HostState::RolledBack);
    let summary = view.summary.as_ref().expect("summary");
    assert!(!summary.ok);
    assert_eq!(summary.head, "deploy FAILED (exit 1)");
}

#[tokio::test]
async fn owned_completed_dismissal_fires_the_drift_refresh() {
    registry_env();
    let cfg = stub_cfg(&["sh", "-c", "exit 0"]);
    let app = drive(
        App::new(filled_state(), cfg),
        vec![
            (0, KeyCode::Char('D')),
            (100, KeyCode::Char('y')),
            (1200, KeyCode::Esc),
        ],
        200,
    )
    .await;
    assert!(app.state.screen.is_none());
    assert!(
        app.state.surveying,
        "completed deploy must auto-refresh drift"
    );
}

#[tokio::test]
async fn deploy_tab_keys_jump_and_cycle() {
    registry_env();
    let cfg = stub_cfg(&["sh", "-c", "sleep 5"]);
    let app = drive(
        App::new(filled_state(), cfg),
        vec![
            (0, KeyCode::Char('D')),
            (100, KeyCode::Char('y')),
            (300, KeyCode::Char('p')), // playbook tab
        ],
        200,
    )
    .await;
    let Some(ScreenState::Deploy(view)) = &app.state.screen else {
        panic!("deploy screen not up");
    };
    assert_eq!(view.active, DeployTab::Playbook);

    // b jumps back to the build tab; s is a no-op before the summary exists.
    let cfg = stub_cfg(&["sh", "-c", "sleep 5"]);
    let app = drive(
        App::new(filled_state(), cfg),
        vec![
            (0, KeyCode::Char('D')),
            (100, KeyCode::Char('y')),
            (300, KeyCode::Char('p')),
            (350, KeyCode::Char('s')),
            (400, KeyCode::Char('b')),
        ],
        200,
    )
    .await;
    let Some(ScreenState::Deploy(view)) = &app.state.screen else {
        panic!("deploy screen not up");
    };
    assert_eq!(view.active, DeployTab::Build);
}

// ---- attached mode (a run launched elsewhere) -------------------------------

fn make_run(meta_pairs: &[(&str, Value)]) -> (String, PathBuf) {
    registry_env();
    let (run_id, path) = registry::new_run_dir().unwrap();
    let mut meta = Meta::new();
    meta.insert("run_id".into(), Value::from(run_id.clone()));
    for (k, v) in meta_pairs {
        meta.insert((*k).to_string(), v.clone());
    }
    registry::write_meta(&path, &meta).unwrap();
    (run_id, path)
}

#[tokio::test]
async fn attached_deploy_tails_without_launching_and_esc_detaches() {
    // A live run (the recorded pid is us): the observer renders its events,
    // never launches anything, and esc DETACHES with rc None (still
    // running) — no drift refresh.
    let (run_id, path) = make_run(&[
        ("limit", Value::from("web,db")),
        ("dry_activate", Value::from(false)),
        ("pid", Value::from(i64::from(std::process::id()))),
    ]);
    write_events(
        &path.join("web.jsonl"),
        &milestones("web", &["eval", "copy"]),
    );
    let run = DeployRun::attach(&run_id).expect("attachable run");
    let mut app = App::new(filled_state(), stub_cfg(&["sh", "-c", "true"]));
    assert!(app.start_deploy(run, false, true, true, (100, 20)).await);
    {
        let Some(ScreenState::Deploy(view)) = &app.state.screen else {
            panic!("deploy screen not up");
        };
        assert!(view.attached);
        assert!(!view.finished, "live pid → still running");
        assert_eq!(view.hosts[0].name, "web");
        assert_eq!(view.hosts[0].state, HostState::Copying);
    }
    let app = drive(app, vec![(300, KeyCode::Esc)], 200).await;
    assert!(app.state.screen.is_none());
    assert!(
        !app.state.surveying,
        "detaching a live run refreshes nothing"
    );
}

#[tokio::test]
async fn attached_settled_run_dismisses_with_derived_rc() {
    // A dead run with a rolled-back host: the attached returncode derives
    // from the sticky terminal states (⇒ 1), so esc fires the refresh.
    let (run_id, path) = make_run(&[
        ("limit", Value::from("web")),
        ("pid", Value::from(999_999_999_i64)), // beyond pid_max: dead
        ("rc", Value::from(0)),
    ]);
    write_events(
        &path.join("web.jsonl"),
        &milestones("web", &["eval", "activate", "rollback"]),
    );
    let run = DeployRun::attach(&run_id).expect("attachable run");
    let mut app = App::new(filled_state(), stub_cfg(&["sh", "-c", "true"]));
    assert!(app.start_deploy(run, false, true, true, (100, 20)).await);
    {
        let Some(ScreenState::Deploy(view)) = &app.state.screen else {
            panic!("deploy screen not up");
        };
        // The finish transition already ran at push (pid gone): summary up.
        assert!(view.finished);
        assert_eq!(view.returncode, Some(1));
        assert_eq!(view.active, DeployTab::Summary);
        assert_eq!(view.hosts[0].state, HostState::RolledBack);
    }
    let app = drive(app, vec![(200, KeyCode::Esc)], 200).await;
    assert!(app.state.screen.is_none());
    assert!(
        app.state.surveying,
        "a settled attached run refreshes drift on close"
    );
}

// ---- standalone (`mandala tui deploy`) --------------------------------------

#[tokio::test]
async fn standalone_exit_code_is_the_run_rc() {
    registry_env();
    let mut run = DeployRun::new("web");
    run.program = Some(vec!["sh".into(), "-c".into(), "exit 3".into()]);
    let mut app = App::new(AppState::new(), ExplorerConfig::default());
    assert!(app.start_deploy(run, true, false, false, (100, 20)).await);
    // The run finishes; esc exits the app with the rc (no explorer under a
    // standalone screen).
    let app = drive(app, vec![(1000, KeyCode::Esc)], 100).await;
    assert_eq!(app.exit_code, Some(3));
}

#[tokio::test]
async fn standalone_operator_cancel_exits_zero() {
    registry_env();
    let mut run = DeployRun::new("web");
    run.program = Some(vec!["sh".into(), "-c".into(), "sleep 2".into()]);
    let mut app = App::new(AppState::new(), ExplorerConfig::default());
    assert!(app.start_deploy(run, true, false, false, (100, 20)).await);
    // esc before completion: DETACH + exit 0 (the run continues — the
    // re-attach notice needs a registry run id, which the raw stub never
    // registers, so only the exit contract is asserted here).
    let app = drive(app, vec![(400, KeyCode::Esc)], 100).await;
    assert_eq!(app.exit_code, Some(0));
}

// ---- runs list: the foreground verb (bryan/nixspace#123) --------------------

/// A settled run stays re-attachable any number of times: open the runs
/// list (`a`), attach (`enter`), detach (`esc`), and do it again — the
/// retired once-only auto-attach set must not block manual attachment.
#[tokio::test]
async fn runs_list_reattaches_a_detached_run_repeatedly() {
    let (run_id, path) = make_run(&[
        ("limit", Value::from("web")),
        ("dry_activate", Value::from(false)),
        ("pid", Value::from(999_999_991)), // dead → settled
        ("rc", Value::from(0)),
    ]);
    write_events(
        &path.join("web.jsonl"),
        &milestones("web", &["eval", "activate", "confirm"]),
    );

    // One script, two attach rounds (a closed event stream quits the app,
    // so the round trip must happen inside a single drive): attach, detach,
    // attach AGAIN — the second attach proves detaching never consumes a
    // run's attachability.
    let app = drive(
        App::new(filled_state(), stub_cfg(&["sh", "-c", "true"])),
        vec![
            (0, KeyCode::Char('a')),
            (150, KeyCode::Enter),
            (300, KeyCode::Esc),
            (450, KeyCode::Char('a')),
            (600, KeyCode::Enter),
        ],
        300,
    )
    .await;
    // The registry is shared across this parallel suite, so cursor 0 may be
    // ANY test's newest run — the invariant under test is that the second
    // attach opened an observer screen at all (detach never consumes
    // attachability). The identity-exact rendering is covered by
    // `attached_deploy_tails_without_launching_and_esc_detaches`.
    match &app.state.screen {
        Some(ScreenState::Deploy(view)) => assert!(view.attached, "attached mode"),
        Some(ScreenState::AttachedLog(_)) => {}
        other => panic!("re-attach after detach must open an observer screen, got {other:?}"),
    }
    // The fixture run is listed and re-listable.
    assert!(
        mandala_tui::screen::RunsState::load()
            .rows
            .iter()
            .any(|row| row.run_id == run_id),
        "the fixture run must appear in the runs list"
    );
}
