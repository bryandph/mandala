//! Screen-tier behavior tests — the `tasks.py` / `deploy.py` (view half)
//! parity coverage, driving the pure screen states directly (no terminal,
//! no fleet, no real ansible/nix/nom).
//!
//! Covered here: confirm/reboot modal semantics (keys, defaults, dismissal
//! payloads, the exact rendered radio text), the after-mutation drift rule,
//! `DeployViewState` event-driven rendering over EventTailer fixtures (host
//! tabs sorted, milestone restyles, sticky states, recap, the summary tab
//! with PLAY RECAP verbatim, the build line), and the attached-log screen's
//! tail/liveness/detach semantics against a private run registry. The App
//! loop flows live in `tests/action_loop.rs` / `tests/deploy_flow.rs`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use mandala_core::registry::{self, Meta};
use mandala_core::runner::{COMMAND_LOG, EventTailer, HostState};
use mandala_tui::render::render;
use mandala_tui::screen::{
    AttachedLogState, ConfirmAction, ConfirmState, DeployTab, DeployViewState, LogLine, ORDERS,
    RebootState, ScreenState, TaskState, attached_close_rc, attached_pump, confirm_lines,
    deploy_tabs, host_state_glyph, host_state_style, reboot_lines,
};
use mandala_tui::state::AppState;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use ratatui::text::Line;
use serde_json::{Value, json};

// ---- helpers ----------------------------------------------------------------

/// The one process-wide registry base: `MANDALA_FLEET_STATE` is process
/// state, so every registry-touching test funnels through this OnceLock
/// (set exactly once, before any test reads it — a private tmp dir, never
/// the operator's real state).
fn registry_env() -> &'static PathBuf {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    BASE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("mandala-screens-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: set once, under the OnceLock, before any concurrent read
        // in this process (every user calls registry_env() first).
        unsafe {
            std::env::set_var("MANDALA_FLEET_STATE", &dir);
            std::env::set_var("MANDALA_FLEET_RUN_KEEP", "500");
        }
        dir
    })
}

fn tmp() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mandala-screens-fixture-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn recorded_run(emitter: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../mandala-core/tests/fixtures/deploy-runs")
        .join(emitter)
}

/// Append v1 events (defaulting `v`/`ts`) to a `.jsonl` file.
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

/// Flatten a rendered line's spans to plain text.
fn line_text(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

fn lines_text(lines: &[Line]) -> Vec<String> {
    lines.iter().map(line_text).collect()
}

// ---- reboot modal semantics (the _ORDERS table + keys) ----------------------

#[test]
fn reboot_defaults_and_choice_mapping() {
    let mut modal = RebootState::new("web,cache");
    // Defaults: serial order, drain ON.
    assert_eq!(modal.order, '1');
    assert!(modal.drain);
    assert_eq!(modal.choice().serial, "1");
    assert!(modal.choice().drain);
    // 2 → rolling, 3 → 100% (the portable "all in one batch").
    modal.set_order('2');
    assert_eq!(modal.choice().serial, "2");
    modal.set_order('3');
    assert_eq!(modal.choice().serial, "100%");
    // Unknown keys are ignored.
    modal.set_order('9');
    assert_eq!(modal.order, '3');
    // d toggles drain both ways.
    modal.toggle_drain();
    assert!(!modal.choice().drain);
    modal.toggle_drain();
    assert!(modal.choice().drain);
}

#[test]
fn orders_table_is_verbatim() {
    assert_eq!(
        ORDERS,
        [
            ('1', "Serial — one host at a time", "1"),
            ('2', "Rolling — 2 hosts in flight", "2"),
            ('3', "All-at-once — every targeted host together", "100%"),
        ]
    );
}

#[test]
fn reboot_modal_renders_the_exact_radio_text() {
    let mut modal = RebootState::new("web");
    assert_eq!(
        lines_text(&reboot_lines(&modal)),
        vec![
            "Reboot 'web'?",
            "",
            "Order (1/2/3)",
            "  ● Serial — one host at a time",
            "  ○ Rolling — 2 hosts in flight",
            "  ○ All-at-once — every targeted host together",
            "",
            "k8s (d)",
            "  ● Drain-safe: cordon & drain k8s nodes first",
            "",
            "y to run   esc to cancel",
        ]
    );
    // Flip order + drain: radios move, the drain caption swaps.
    modal.set_order('3');
    modal.toggle_drain();
    let lines = reboot_lines(&modal);
    assert_eq!(line_text(&lines[3]), "  ○ Serial — one host at a time");
    assert_eq!(
        line_text(&lines[5]),
        "  ● All-at-once — every targeted host together"
    );
    assert_eq!(
        line_text(&lines[8]),
        "  ○ Skip drain: reboot k8s nodes without draining"
    );
}

#[test]
fn reboot_modal_radio_styles_track_selection() {
    let modal = RebootState::new("web");
    let lines = reboot_lines(&modal);
    // Selected radio: bold green glyph, bold label.
    let on = &lines[3].spans;
    assert_eq!(on[1].content.as_ref(), "●");
    assert_eq!(on[1].style.fg, Some(Color::Green));
    assert!(on[1].style.add_modifier.contains(Modifier::BOLD));
    assert!(on[2].style.add_modifier.contains(Modifier::BOLD));
    // Unselected: dim glyph and label.
    let off = &lines[4].spans;
    assert_eq!(off[1].content.as_ref(), "○");
    assert!(off[1].style.add_modifier.contains(Modifier::DIM));
    assert!(off[2].style.add_modifier.contains(Modifier::DIM));
    // The run key is bold red.
    let trailer = &lines[10].spans;
    assert_eq!(trailer[0].content.as_ref(), "y");
    assert_eq!(trailer[0].style.fg, Some(Color::Red));
}

#[test]
fn confirm_modal_renders_message_and_trailer() {
    let confirm = ConfirmState::new(
        "Deploy 'web'?\n(eval-once batch build, then deploy-rs per host with magic rollback)",
        ConfirmAction::Deploy {
            target: "web".into(),
        },
    );
    assert_eq!(
        lines_text(&confirm_lines(&confirm)),
        vec![
            "Deploy 'web'?",
            "(eval-once batch build, then deploy-rs per host with magic rollback)",
            "",
            "y to run   esc to cancel",
        ]
    );
}

// ---- the after-mutation drift rule ------------------------------------------

#[test]
fn after_mutation_refreshes_on_rc_some_only() {
    // rc Some — even non-zero: seeing the resulting state is the point.
    let mut state = AppState::new();
    assert_eq!(state.after_mutation(Some(1)), (true, true));
    assert!(state.busy && state.surveying);
    // rc None = operator cancel: nothing starts.
    let mut state = AppState::new();
    assert_eq!(state.after_mutation(None), (false, false));
    assert!(!state.busy && !state.surveying);
    // A sticky error clears on the refresh, exactly like `S`.
    let mut state = AppState::new();
    state.set_status("eval failed: boom", true);
    let _ = state.after_mutation(Some(0));
    assert!(!state.status_sticky);
}

// ---- task state semantics ---------------------------------------------------

#[test]
fn task_exit_records_rc_and_trailer() {
    let mut task = TaskState::new("ping web", 1, false);
    task.push_line("$ ansible web -m ping  (cwd=ansible)".into());
    task.on_exited(3);
    assert_eq!(task.rc, Some(3));
    assert_eq!(task.lines.back().unwrap(), "— exit 3");
}

// ---- DeployViewState over EventTailer fixtures ------------------------------

#[test]
fn host_tabs_appear_sorted_and_restyle_on_milestones() {
    let dir = tmp();
    // beta first on disk; the tailer's BTreeMap sorts the tabs.
    write_events(&dir.join("beta.jsonl"), &milestones("beta", &["eval"]));
    write_events(&dir.join("alpha.jsonl"), &milestones("alpha", &["copy"]));
    let mut tailer = EventTailer::new(&dir);
    tailer.poll();

    let mut view = DeployViewState::new("alpha,beta", false, false, false, true);
    view.sync(Some(&tailer), &[], false, None, 0);
    let names: Vec<&str> = view.hosts.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, ["alpha", "beta"]);
    assert_eq!(view.hosts[0].state, HostState::Copying);
    assert_eq!(host_state_glyph(view.hosts[0].state), "⇄");
    assert_eq!(view.hosts[1].state, HostState::Evaluating);
    assert_eq!(
        deploy_tabs(&view),
        vec![
            DeployTab::Build,
            DeployTab::Playbook,
            DeployTab::Host("alpha".into()),
            DeployTab::Host("beta".into()),
        ]
    );

    // A milestone transition restyles the label on the next poll.
    write_events(
        &dir.join("alpha.jsonl"),
        &milestones("alpha", &["activate", "confirm"]),
    );
    tailer.poll();
    view.sync(Some(&tailer), &[], false, None, 0);
    assert_eq!(view.hosts[0].state, HostState::Confirmed);
    assert_eq!(host_state_glyph(view.hosts[0].state), "✓");
    assert_eq!(host_state_style(view.hosts[0].state).fg, Some(Color::Green));
}

#[test]
fn sticky_rollback_survives_a_late_done_rc() {
    let dir = tmp();
    write_events(
        &dir.join("beta.jsonl"),
        &milestones("beta", &["eval", "activate", "rollback"]),
    );
    write_events(
        &dir.join("beta.jsonl"),
        &[json!({"host":"beta","plugin":"deploy","event":"status","state":"done","rc":1})],
    );
    let mut tailer = EventTailer::new(&dir);
    tailer.poll();
    let mut view = DeployViewState::new("beta", false, false, false, true);
    view.sync(Some(&tailer), &[], false, None, 0);
    assert_eq!(view.hosts[0].state, HostState::RolledBack);
    assert_eq!(view.hosts[0].rc, Some(1));
    assert_eq!(host_state_glyph(HostState::RolledBack), "↩");
    let style = host_state_style(HostState::RolledBack);
    assert_eq!(style.fg, Some(Color::Red));
    assert!(style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn build_line_tracks_progress_then_done() {
    let dir = tmp();
    write_events(
        &dir.join("alpha.jsonl"),
        &[json!({"host":"alpha","plugin":"build","event":"progress",
            "built":4,"finished":2,"fetched":9,"fetched_done":7,"errors":1,"current":"system-path"})],
    );
    let mut tailer = EventTailer::new(&dir);
    tailer.poll();
    let mut view = DeployViewState::new("alpha", false, false, false, true);
    view.sync(Some(&tailer), &[], false, None, 0);
    assert_eq!(
        view.build_line,
        "batch build  built 2/4  fetched 7/9  errors 1  —  system-path"
    );
    write_events(
        &dir.join("alpha.jsonl"),
        &[json!({"host":"alpha","plugin":"build","event":"status","state":"done","rc":0})],
    );
    tailer.poll();
    view.sync(Some(&tailer), &[], false, None, 0);
    assert_eq!(
        view.build_line,
        "batch build  built 2/4  fetched 7/9  errors 1  —  done rc=0"
    );
}

#[test]
fn summary_materializes_once_with_play_recap_verbatim() {
    let dir = tmp();
    write_events(
        &dir.join("alpha.jsonl"),
        &[
            json!({"host":"alpha","plugin":"build","event":"progress",
                "built":3,"finished":3,"fetched":5,"fetched_done":5,"errors":0,"current":""}),
            json!({"host":"alpha","plugin":"build","event":"status","state":"done","rc":0}),
        ],
    );
    write_events(
        &dir.join("alpha.jsonl"),
        &milestones("alpha", &["eval", "activate", "confirm"]),
    );
    write_events(
        &dir.join("beta.jsonl"),
        &milestones("beta", &["eval", "activate", "rollback"]),
    );
    let mut tailer = EventTailer::new(&dir);
    tailer.poll();

    let output = vec![
        "TASK [deploy] *****".to_string(),
        "PLAY RECAP *****".to_string(),
        "alpha : ok=3 changed=1 failed=0".to_string(),
        "beta : ok=2 changed=1 failed=1".to_string(),
    ];
    let mut view = DeployViewState::new("alpha,beta", true, false, false, true);
    view.sync(Some(&tailer), &output, true, Some(2), 125);

    // The finish transition: sub-title gains the exit, the summary tab
    // materializes and takes focus.
    assert!(view.finished);
    assert_eq!(view.sub_title(), "-l alpha,beta (dry-activate) — exit 2");
    assert_eq!(view.active, DeployTab::Summary);
    let summary = view.summary.clone().expect("summary materialized");
    assert_eq!(summary.head, "deploy FAILED (exit 2)");
    assert!(!summary.ok);
    assert_eq!(summary.meta, "   -l alpha,beta   2m05s   dry-activate");
    assert_eq!(
        summary.build_line,
        "batch build: built 3/3, fetched 5/5, errors 0, rc 0"
    );
    assert!(!summary.build_bad);
    // Host table rows, sorted, with per-state rc.
    assert_eq!(summary.hosts.len(), 2);
    assert_eq!(summary.hosts[0].0, "alpha");
    assert_eq!(summary.hosts[0].1, HostState::Confirmed);
    assert_eq!(summary.hosts[1].1, HostState::RolledBack);
    // ansible's own accounting, verbatim from the PLAY RECAP line on.
    assert_eq!(
        summary.recap,
        vec![
            "PLAY RECAP *****",
            "alpha : ok=3 changed=1 failed=0",
            "beta : ok=2 changed=1 failed=1",
        ]
    );

    // ONCE-only: a later sync neither rebuilds the summary nor steals focus.
    view.active = DeployTab::Build;
    view.sync(Some(&tailer), &output, true, Some(2), 999);
    assert_eq!(view.active, DeployTab::Build);
    assert_eq!(
        view.summary.unwrap().meta,
        "   -l alpha,beta   2m05s   dry-activate"
    );
}

#[test]
fn summary_flags_a_bad_build_rc_and_success_head() {
    let dir = tmp();
    write_events(
        &dir.join("alpha.jsonl"),
        &[json!({"host":"alpha","plugin":"build","event":"status","state":"done","rc":1})],
    );
    let mut tailer = EventTailer::new(&dir);
    tailer.poll();
    let mut view = DeployViewState::new("alpha", false, false, false, true);
    view.sync(Some(&tailer), &[], true, Some(0), 43);
    let summary = view.summary.unwrap();
    assert!(summary.build_bad); // rc 1 renders red
    assert_eq!(summary.head, "deploy succeeded");
    assert!(summary.ok);
    assert_eq!(summary.meta, "   -l alpha   0m43s");
    assert!(summary.recap.is_empty()); // no PLAY RECAP in the mirror
}

// ---- attached-log screen (private run registry) -----------------------------

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

fn raw_texts(state: &AttachedLogState) -> Vec<String> {
    state
        .lines
        .iter()
        .map(|l| match l {
            LogLine::Raw(s) => s.clone(),
            LogLine::Notice { text, .. } => format!("[{text}]"),
        })
        .collect()
}

#[test]
fn attached_pump_tails_by_offset_while_running() {
    let (run_id, path) = make_run(&[
        ("kind", Value::from("reboot")),
        ("pid", Value::from(i64::from(std::process::id()))), // alive: us
    ]);
    std::fs::write(path.join(COMMAND_LOG), "$ ans-reboot -l web\nline-1\n").unwrap();
    let mut state = AttachedLogState::new("reboot web", run_id, true);
    attached_pump(&mut state);
    assert_eq!(raw_texts(&state), ["$ ans-reboot -l web", "line-1"]);
    assert!(!state.settled);
    // Only the appended tail is read on the next pump.
    let mut fh = std::fs::OpenOptions::new()
        .append(true)
        .open(path.join(COMMAND_LOG))
        .unwrap();
    writeln!(fh, "line-2").unwrap();
    attached_pump(&mut state);
    assert_eq!(
        raw_texts(&state),
        ["$ ans-reboot -l web", "line-1", "line-2"]
    );
}

#[test]
fn attached_pump_settles_with_liveness_trailer() {
    // A reaped command run: pid gone, rc recorded → failed trailer, once.
    let (run_id, path) = make_run(&[
        ("kind", Value::from("reboot")),
        ("pid", Value::Null),
        ("rc", Value::from(3)),
    ]);
    std::fs::write(path.join(COMMAND_LOG), "boom\n").unwrap();
    let mut state = AttachedLogState::new("reboot web", run_id.clone(), true);
    attached_pump(&mut state);
    assert!(state.settled);
    let Some(LogLine::Notice { text, error }) = state.lines.back() else {
        panic!("no trailer: {:?}", state.lines);
    };
    assert_eq!(text, "— failed (rc=3)");
    assert!(*error);
    // A second pump appends nothing new.
    let n = state.lines.len();
    attached_pump(&mut state);
    assert_eq!(state.lines.len(), n);

    // The clean twin styles green (error=false).
    let (ok_id, ok_path) = make_run(&[("pid", Value::Null), ("rc", Value::from(0))]);
    std::fs::write(ok_path.join(COMMAND_LOG), "done\n").unwrap();
    let mut ok = AttachedLogState::new("reboot web", ok_id, true);
    attached_pump(&mut ok);
    let Some(LogLine::Notice { text, error }) = ok.lines.back() else {
        panic!("no trailer");
    };
    assert_eq!(text, "— finished (rc=0)");
    assert!(!*error);
}

#[test]
fn attached_pump_reports_a_pruned_run() {
    registry_env();
    let mut state = AttachedLogState::new("reboot web", "nonesuch-run", true);
    attached_pump(&mut state);
    assert!(state.settled);
    let Some(LogLine::Notice { text, error }) = state.lines.back() else {
        panic!("no notice");
    };
    assert_eq!(text, "run nonesuch-run is gone (pruned?)");
    assert!(*error);
}

#[test]
fn attached_close_rc_only_once_settled() {
    // Still running (live pid): the dismissal rc is None — detaching from a
    // live run must not fire the drift refresh.
    let (live_id, _) = make_run(&[("pid", Value::from(i64::from(std::process::id())))]);
    assert_eq!(attached_close_rc(&live_id), None);
    // Settled: the recorded rc rides the dismissal.
    let (done_id, _) = make_run(&[("pid", Value::Null), ("rc", Value::from(2))]);
    assert_eq!(attached_close_rc(&done_id), Some(2));
    // Gone entirely: None (nothing completed from this screen's view).
    assert_eq!(attached_close_rc("nonesuch-run"), None);
}

// ---- render grids (TestBackend + insta) -------------------------------------

fn draw(state: &AppState, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal
        .draw(|frame| render(state, frame))
        .expect("render screen state");
    terminal
}

#[test]
fn snapshot_reboot_modal() {
    let mut state = AppState::new();
    state.screen = Some(ScreenState::Reboot(RebootState::new("web,cache")));
    let terminal = draw(&state, 90, 16);
    insta::assert_snapshot!(terminal.backend());
}

#[test]
fn snapshot_confirm_modal() {
    let mut state = AppState::new();
    state.screen = Some(ScreenState::Confirm(ConfirmState::new(
        "Deploy 'web'?\n(eval-once batch build, then deploy-rs per host with magic rollback)",
        ConfirmAction::Deploy {
            target: "web".into(),
        },
    )));
    let terminal = draw(&state, 90, 12);
    insta::assert_snapshot!(terminal.backend());
}

#[test]
fn snapshot_task_screen_stream() {
    let mut task = TaskState::new("ping web,cache", 1, false);
    task.push_line("$ ansible web,cache -m ping  (cwd=ansible)".into());
    task.push_line("web | SUCCESS => {".into());
    task.push_line("    \"ping\": \"pong\"".into());
    task.push_line("}".into());
    task.on_exited(0);
    let mut state = AppState::new();
    state.screen = Some(ScreenState::Task(task));
    let terminal = draw(&state, 80, 10);
    insta::assert_snapshot!(terminal.backend());
}

#[test]
fn snapshot_deploy_screen_running_and_summary() {
    let dir = tmp();
    write_events(
        &dir.join("alpha.jsonl"),
        &[json!({"host":"alpha","plugin":"build","event":"progress",
            "built":4,"finished":2,"fetched":9,"fetched_done":9,"errors":0,"current":"system-path"})],
    );
    write_events(
        &dir.join("alpha.jsonl"),
        &milestones("alpha", &["eval", "copy"]),
    );
    write_events(
        &dir.join("beta.jsonl"),
        &milestones("beta", &["eval", "activate", "rollback"]),
    );
    let mut tailer = EventTailer::new(&dir);
    tailer.poll();
    let mut view = DeployViewState::new("alpha,beta", false, false, false, true);
    view.sync(
        Some(&tailer),
        &["TASK [fanout] *****".to_string()],
        false,
        None,
        0,
    );
    view.active = DeployTab::Host("beta".into());
    let mut state = AppState::new();
    state.screen = Some(ScreenState::Deploy(view.clone()));
    let terminal = draw(&state, 100, 12);
    insta::assert_snapshot!("deploy_screen_running", terminal.backend());

    // The finished twin: summary tab focused, PLAY RECAP verbatim.
    let output = vec![
        "PLAY RECAP *****".to_string(),
        "alpha : ok=3 failed=0".to_string(),
        "beta : ok=2 failed=1".to_string(),
    ];
    view.sync(Some(&tailer), &output, true, Some(1), 83);
    let mut state = AppState::new();
    state.screen = Some(ScreenState::Deploy(view));
    let terminal = draw(&state, 100, 16);
    insta::assert_snapshot!("deploy_screen_summary", terminal.backend());
}

/// The real TUI view and renderer are emitter-agnostic: the retired Ansible
/// recording puts build events in `alpha.jsonl`, while the native engine uses
/// `build.jsonl`, yet both must produce the same host tabs, build summary, and
/// finished screen without any consumer-side special case.
#[test]
fn recorded_ansible_and_engine_runs_render_identically() {
    let render_recording = |emitter: &str| {
        let mut tailer = EventTailer::new(&recorded_run(emitter));
        assert_eq!(tailer.poll(), 18);
        let mut view = DeployViewState::new("alpha,beta", false, false, true, true);
        view.sync(Some(&tailer), &[], true, Some(1), 11);
        assert_eq!(view.hosts[0].state, HostState::Confirmed);
        assert_eq!(view.hosts[1].state, HostState::RolledBack);
        assert_eq!(view.hosts[1].rc, Some(1));
        assert_eq!(
            view.build_line,
            "batch build  built 2/2  fetched 1/1  errors 0  —  done rc=0"
        );
        let mut state = AppState::new();
        state.screen = Some(ScreenState::Deploy(view));
        format!("{}", draw(&state, 100, 16).backend())
    };

    let ansible = render_recording("ansible");
    let engine = render_recording("engine");
    assert_eq!(engine, ansible);
    assert!(engine.contains("deploy FAILED (exit 1)"));
    assert!(engine.contains("alpha"));
    assert!(engine.contains("rolled-back"));
}

/// Host tab labels carry the state style in the tab bar (snapshots can't
/// see styles — direct buffer-cell assertion, the harness pattern).
#[test]
fn deploy_tab_bar_styles_host_labels() {
    let dir = tmp();
    write_events(
        &dir.join("beta.jsonl"),
        &milestones("beta", &["eval", "activate", "rollback"]),
    );
    let mut tailer = EventTailer::new(&dir);
    tailer.poll();
    let mut view = DeployViewState::new("beta", false, false, false, true);
    view.sync(Some(&tailer), &[], false, None, 0);
    let mut state = AppState::new();
    state.screen = Some(ScreenState::Deploy(view));
    let terminal = draw(&state, 100, 10);
    let buf = terminal.backend().buffer();
    // Row 2 is the tab bar: " ⚙ build │ ansible │ ↩ beta ". Find the ↩.
    let row = 2u16;
    let mut found = false;
    for x in 0..100u16 {
        let cell = buf.cell((x, row)).expect("tab bar cell");
        if cell.symbol() == "↩" {
            assert_eq!(cell.style().fg, Some(Color::Red));
            assert!(cell.style().add_modifier.contains(Modifier::BOLD));
            found = true;
        }
    }
    assert!(found, "rolled-back glyph not rendered in the tab bar");
}

/// The recap strip: waiting notice with no hosts, glyph name:state per host.
#[test]
fn recap_strip_contents() {
    let empty = DeployViewState::new("web", false, false, false, true);
    let mut state = AppState::new();
    state.screen = Some(ScreenState::Deploy(empty));
    let terminal = draw(&state, 80, 8);
    let text = format!("{}", terminal.backend());
    assert!(text.contains("waiting for host events…"));

    let dir = tmp();
    write_events(
        &dir.join("web.jsonl"),
        &milestones("web", &["eval", "copy"]),
    );
    let mut tailer = EventTailer::new(&dir);
    tailer.poll();
    let mut view = DeployViewState::new("web", false, false, false, true);
    view.sync(Some(&tailer), &[], false, None, 0);
    let mut state = AppState::new();
    state.screen = Some(ScreenState::Deploy(view));
    let terminal = draw(&state, 80, 8);
    let text = format!("{}", terminal.backend());
    assert!(
        text.contains("⇄ web:copying"),
        "recap strip missing: {text}"
    );
}
