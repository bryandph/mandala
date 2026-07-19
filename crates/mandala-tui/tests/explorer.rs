//! Explorer-tier behavior tests — the `test_tui_mcp.py` analog, driving
//! AppState transitions directly (no terminal, no fleet, no real ansible).
//!
//! Every case here is a design "hard-won behavior": sticky status errors,
//! reload queued-not-dropped, the stale-aggregate guard, the three drift
//! captions, selection→target semantics, the concurrent-jobs spinner line,
//! and the survey's fresh-snapshot tally (dot-files skipped). The survey
//! PIPELINE runs against `sh -c` stubs (the phase-1 runner-test pattern).

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use mandala_core::drift::Snapshot;
use mandala_core::inventory::Inventory;
use mandala_tui::event::AppEvent;
use mandala_tui::explorer::{fresh_snapshots, run_survey};
use mandala_tui::state::{AppState, LoadedInventory, Tab, drift_hint};
use serde_json::json;

// ---- fixtures ---------------------------------------------------------------

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

fn loaded(rev: Option<&str>, cached_rev: Option<&str>) -> LoadedInventory {
    let mut cached = BTreeMap::new();
    if cached_rev.is_some() {
        cached.insert(
            "cache".to_string(),
            "/nix/store/cached00-toplevel".to_string(),
        );
    }
    LoadedInventory {
        inventory: Inventory::from_value(aggregate()).expect("fixture aggregate is valid"),
        rev: rev.map(str::to_string),
        cached_rev: cached_rev.map(str::to_string),
        cached,
    }
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-06-12T12:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc)
}

fn snapshots() -> BTreeMap<String, Snapshot> {
    let snap = |current: &str| -> Snapshot {
        serde_json::from_value(json!({
            "current": current,
            "booted": current,
            "captured_at": "2026-06-12T11:59:00+00:00",
        }))
        .expect("snapshot fixture parses")
    };
    BTreeMap::from([
        ("cache".to_string(), snap("/nix/store/curcache-toplevel")),
        ("web".to_string(), snap("/nix/store/curweb00-toplevel")),
    ])
}

fn filled_state() -> AppState {
    let mut state = AppState::new();
    let req = state.request_load().expect("idle state loads");
    let follow_up = state.on_load_finished(
        req.generation,
        Ok(loaded(Some("aaaaaaaaaaaaaaaa"), None)),
        &snapshots(),
        now(),
    );
    assert!(follow_up.is_none());
    state
}

fn tmp() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mandala-tui-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---- selection → target (feeds the section-5 actions) -----------------------

#[test]
fn target_is_selection_else_cursor_on_the_active_tab() {
    let mut state = filled_state();
    // No selection: the cursor row (members sort: cache, router, web).
    assert_eq!(state.target().as_deref(), Some("cache"));
    // A multi-selection wins, comma-joined in TABLE order.
    state.members_table.toggle(); // cache
    state.members_table.skip(2); // cursor on web — selection unchanged
    state.members_table.toggle(); // + web
    assert_eq!(state.target().as_deref(), Some("cache,web"));
    // The target follows the ACTIVE tab: groups has its own table/cursor.
    state.tab = Tab::Groups;
    assert_eq!(state.target().as_deref(), Some("gateway"));
    state.groups_table.move_cursor(1);
    assert_eq!(state.target().as_deref(), Some("k3s"));
    // Back on members the selection still holds (selection is per-table).
    state.tab = Tab::Members;
    assert_eq!(state.target().as_deref(), Some("cache,web"));
    // An empty explorer has no target at all.
    assert_eq!(AppState::new().target(), None);
}

// ---- status machinery -------------------------------------------------------

#[test]
fn sticky_error_survives_a_concurrent_success() {
    let mut state = filled_state();
    let (eval, survey) = state.refresh_drift();
    assert!(eval && survey);
    // The eval fails while the survey is still running…
    let follow_up =
        state.on_drift_eval_finished(Err("eval failed: boom".to_string()), &snapshots(), now());
    assert!(follow_up.is_none());
    // …the bar still shows the survey spinner (the error is the RESTING msg).
    assert!(state.status_line().contains("survey"));
    // The survey's success must NOT stomp the sticky error.
    state.on_survey_done(2, 0, None, &snapshots(), now());
    assert_eq!(state.status, "eval failed: boom");
    assert_eq!(state.status_line(), "eval failed: boom");
    // The eval error also dropped the expected set (Python `_drift_done`).
    assert!(state.expected.is_none());
    // The NEXT refresh clears the stickiness…
    let (eval, survey) = state.refresh_drift();
    assert!(eval && survey);
    state.on_survey_done(2, 0, None, &snapshots(), now());
    // …so this survey's success message lands.
    assert_eq!(state.status, "drift refreshed · surveyed 2 hosts");
}

#[test]
fn survey_success_first_keeps_the_eval_spinner_up() {
    let mut state = filled_state();
    let _ = state.refresh_drift();
    // Survey lands first: its resting message is set, but the bar still
    // spins for the eval.
    state.on_survey_done(1, 0, None, &snapshots(), now());
    assert_eq!(state.status, "drift refreshed · surveyed 1 host");
    assert!(state.status_line().starts_with("running   "));
    assert!(state.status_line().contains("eval"));
    // Then the eval error lands and (sticky) replaces the resting message.
    let _ = state.on_drift_eval_finished(Err("eval failed: nope".to_string()), &snapshots(), now());
    assert_eq!(state.status_line(), "eval failed: nope");
}

#[test]
fn spinner_line_lists_every_running_job_with_one_shared_frame() {
    let mut state = filled_state();
    let _ = state.refresh_drift();
    assert_eq!(state.status_line(), "running   ⠋ eval   ·   ⠋ survey");
    state.on_survey_progress(3);
    assert!(state.tick_spinner());
    assert_eq!(
        state.status_line(),
        "running   ⠙ eval   ·   ⠙ survey (3 read)"
    );
    // Idle again: the resting message, and idle ticks change nothing.
    let _ = state.on_drift_eval_finished(
        Ok((Some("aaaaaaaaaaaaaaaa".to_string()), BTreeMap::new())),
        &snapshots(),
        now(),
    );
    state.on_survey_done(3, 0, None, &snapshots(), now());
    assert!(!state.tick_spinner());
    assert_eq!(state.status_line(), "drift refreshed · surveyed 3 hosts");
}

#[test]
fn survey_failure_is_sticky_and_names_the_exit_and_last_line() {
    let mut state = filled_state();
    assert!(state.request_survey());
    state.on_survey_done(0, 2, Some("oops"), &snapshots(), now());
    assert_eq!(state.status, "survey failed (exit 2): oops");
    assert!(state.status_sticky);
    // No captured line: the trailing space is trimmed (Python rstrip).
    let mut state = filled_state();
    assert!(state.request_survey());
    state.on_survey_done(0, 2, None, &snapshots(), now());
    assert_eq!(state.status, "survey failed (exit 2):");
}

#[test]
fn load_failure_surfaces_the_last_error_line_sticky() {
    let mut state = AppState::new();
    let req = state.request_load().unwrap();
    let follow_up = state.on_load_finished(
        req.generation,
        Err("trace line\nerror: attribute missing".to_string()),
        &snapshots(),
        now(),
    );
    assert!(follow_up.is_none());
    assert_eq!(
        state.status,
        "aggregate eval failed: error: attribute missing"
    );
    assert!(state.status_sticky);
    assert!(!state.busy);
}

#[test]
fn eval_expected_never_queues_while_busy() {
    let mut state = filled_state();
    assert!(state.request_eval_expected());
    // A second S while the eval runs is a no-op (returns early on busy)…
    assert!(!state.request_eval_expected());
    // …and does NOT queue anything for later (unlike a reload).
    assert!(!state.reload_pending);
}

// ---- reload queued-not-dropped + the stale-aggregate guard ------------------

#[test]
fn reload_while_busy_queues_and_the_stale_fill_never_paints() {
    let mut state = AppState::new();
    let first = state.request_load().expect("initial load starts");
    assert_eq!(first.generation, 0);
    // `r` while the eval worker runs: queued, not dropped; the inventory is
    // rebound (unevaluated → no deploy nodes) and expected dropped.
    assert!(state.request_reload().is_none());
    assert!(state.reload_pending);
    assert_eq!(state.generation, 1);
    assert!(state.inventory.is_none());
    assert!(state.deploy_nodes().is_empty());
    // The FIRST load lands, carrying the superseded generation: it must not
    // paint, and the queued reload starts with the fresh generation.
    let follow_up = state.on_load_finished(
        first.generation,
        Ok(loaded(Some("aaaaaaaaaaaaaaaa"), None)),
        &snapshots(),
        now(),
    );
    assert_eq!(follow_up.expect("queued reload starts").generation, 1);
    assert!(state.member_rows.is_empty()); // the stale aggregate never painted
    assert!(state.busy); // the queued reload is now in flight
    assert!(!state.reload_pending);
    // The fresh load paints normally.
    let follow_up = state.on_load_finished(
        1,
        Ok(loaded(Some("bbbbbbbbbbbbbbbb"), None)),
        &snapshots(),
        now(),
    );
    assert!(follow_up.is_none());
    assert_eq!(state.member_rows.len(), 3);
    assert_eq!(
        state.status,
        "3 members, 2 groups — space/shift+↑↓ select · p ping · R reboot · D deploy"
    );
}

#[test]
fn reload_queued_behind_an_expected_eval_runs_after_it() {
    let mut state = filled_state();
    assert!(state.request_eval_expected());
    // `r` during the expected eval queues (busy covers BOTH eval classes).
    assert!(state.request_reload().is_none());
    assert!(state.reload_pending);
    // The eval settles → the queued reload starts.
    let follow_up = state.on_drift_eval_finished(
        Ok((Some("aaaaaaaaaaaaaaaa".to_string()), BTreeMap::new())),
        &snapshots(),
        now(),
    );
    assert_eq!(follow_up.expect("queued reload starts").generation, 1);
}

// ---- drift captions (the exact three cases) ---------------------------------

#[test]
fn drift_caption_expected_fresh() {
    let mut state = filled_state();
    assert!(state.request_eval_expected());
    let _ = state.on_drift_eval_finished(
        Ok((
            Some("aaaaaaaaaaaaaaaa".to_string()),
            BTreeMap::from([(
                "cache".to_string(),
                "/nix/store/curcache-toplevel".to_string(),
            )]),
        )),
        &snapshots(),
        now(),
    );
    assert_eq!(
        state.drift_hint,
        "S refresh drift (survey + eval) · R reboot a reboot-pending row   expected @ aaaaaaaaaaa"
    );
    // The rev-keyed cache also satisfies this case on a plain load: same
    // clean rev → the cached expectation is adopted.
    let mut state = AppState::new();
    let _ = state.request_load();
    let _ = state.on_load_finished(
        0,
        Ok(loaded(Some("cccccccccccccccc"), Some("cccccccccccccccc"))),
        &snapshots(),
        now(),
    );
    assert!(state.expected.is_some());
    assert!(state.drift_hint.ends_with("expected @ ccccccccccc"));
}

#[test]
fn drift_caption_contract_moved_since_last_eval() {
    let mut state = AppState::new();
    let _ = state.request_load();
    let _ = state.on_load_finished(
        0,
        Ok(loaded(Some("bbbbbbbbbbbbbbbb"), Some("aaaaaaaaaaaaaaaa"))),
        &snapshots(),
        now(),
    );
    assert!(state.expected.is_none()); // stale cache is NOT adopted
    assert_eq!(
        state.drift_hint,
        "S refresh drift (survey + eval) · R reboot a reboot-pending row   \
         contract MOVED since last eval (cache @ aaaaaaaaaaa, repo @ bbbbbbbbbbb) — press S"
    );
}

#[test]
fn drift_caption_never_evaluated() {
    let state = filled_state();
    assert_eq!(
        state.drift_hint,
        "S refresh drift (survey + eval) · R reboot a reboot-pending row   \
         (expected NOT evaluated yet — press S)"
    );
    // The pure caption fn covers the dirty-rev spelling too.
    assert_eq!(
        drift_hint(true, Some(&format!("{}-dirty", "a".repeat(40))), None),
        "S refresh drift (survey + eval) · R reboot a reboot-pending row   \
         expected @ aaaaaaaaaaa-dirty"
    );
}

// ---- the survey tally + pipeline (sh -c stubs, never real ansible) ----------

#[test]
fn survey_tally_counts_only_fresh_non_dot_snapshots() {
    let dir = tmp();
    std::fs::write(dir.join("alpha.json"), "{}").unwrap();
    std::fs::write(dir.join("beta.json"), "{}").unwrap();
    std::fs::write(dir.join(".expected.json"), "{}").unwrap(); // the eval cache
    std::fs::write(dir.join("notes.txt"), "x").unwrap();
    let past = std::time::SystemTime::UNIX_EPOCH;
    assert_eq!(fresh_snapshots(&dir, past), 2);
    // Snapshots older than the run start don't count as "this run".
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    assert_eq!(fresh_snapshots(&dir, future), 0);
    // A missing dir counts zero (the Python `is_dir` guard).
    assert_eq!(fresh_snapshots(&dir.join("nope"), past), 0);
}

async fn collect_survey(argv: Vec<String>, state_dir: PathBuf) -> Vec<AppEvent> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let cwd = std::env::temp_dir();
    tokio::spawn(run_survey(tx, argv, cwd, state_dir));
    let mut events = Vec::new();
    loop {
        let ev = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .expect("survey settles in time")
            .expect("survey channel stays open until done");
        let done = matches!(ev, AppEvent::SurveyDone { .. });
        events.push(ev);
        if done {
            break;
        }
    }
    events
}

#[tokio::test]
async fn survey_pipeline_tallies_fresh_snapshots_and_settles_clean() {
    let state_dir = tmp();
    let script = r#"echo starting; printf '{}' > "$MANDALA_FLEET_STATE/hostx.json"; echo done"#;
    let events = collect_survey(
        vec!["sh".to_string(), "-c".to_string(), script.to_string()],
        state_dir,
    )
    .await;
    let Some(AppEvent::SurveyDone { n, rc, error }) = events.last() else {
        panic!("survey did not settle: {events:?}");
    };
    assert_eq!((*n, *rc), (1, 0));
    assert!(
        error.is_none(),
        "clean survey surfaces no output: {error:?}"
    );
    // The live tally reported the freshly written snapshot before settling.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AppEvent::SurveyProgress { n: 1 })),
        "no progress tally in {events:?}"
    );
}

#[tokio::test]
async fn survey_pipeline_surfaces_the_last_line_only_on_failure() {
    let events = collect_survey(
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo oops >&2; exit 3".to_string(),
        ],
        tmp(),
    )
    .await;
    let Some(AppEvent::SurveyDone { n, rc, error }) = events.last() else {
        panic!("survey did not settle: {events:?}");
    };
    assert_eq!((*n, *rc), (0, 3));
    assert_eq!(error.as_deref(), Some("oops"));
}

#[tokio::test]
async fn survey_pipeline_reports_a_launch_failure() {
    let events = collect_survey(vec!["/nonexistent/mandala-survey-stub".to_string()], tmp()).await;
    let Some(AppEvent::SurveyDone { n, rc, error }) = events.last() else {
        panic!("survey did not settle: {events:?}");
    };
    assert_eq!((*n, *rc), (0, 1));
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.starts_with("failed to launch: ")),
        "unexpected error: {error:?}"
    );
}

// ---- unevaluated inventory never evals on the UI path -----------------------

#[test]
fn deploy_nodes_are_empty_until_the_background_eval_lands() {
    let mut state = filled_state();
    assert_eq!(state.deploy_nodes(), ["cache", "web"]);
    // `r` rebinds an unevaluated inventory: no nodes, and a drift repaint
    // (e.g. a landing survey) renders an EMPTY table rather than forcing an
    // eval on the UI thread.
    let _ = state.request_reload();
    assert!(state.deploy_nodes().is_empty());
    state.fill_drift(&snapshots(), now());
    assert!(state.drift_rows.is_empty());
}
