//! Direction-A interop golden tests (fleet-state-formats spec, task 2.5):
//! a state directory **written by the Python implementation** through its
//! real code paths — checked in under `cli/tests/fixtures/interop/state/`,
//! regenerated only by `cli/tests/fixtures/interop/generate.py` (the
//! fixtures' provenance; see its README) — read here by the Rust registry,
//! runner, and drift cores. `cli/tests/test_interop_fixtures.py` asserts
//! the PYTHON reading of the same bytes, so both implementations are
//! pinned to one judgement over one tree. Direction B (Rust writes, Python
//! attaches live) is `cli/tests/test_interop_rs.py` + the
//! `mandala-interop-helper` binary.
//!
//! These tests run in plain `cargo test` and the `nix build .#mandala-rs`
//! check phase (the fixture tree is in the package fileset) — no Python
//! needed at cargo-test time.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use serde_json::Value;

use crate::drift::{self, DriftStatus, default_max_age};
use crate::registry::{self, RunLiveness, test_hooks};
use crate::runner::{DeployRun, EventTailer, HostState};

// Fixture run ids (suffix = the fake pid recorded in meta; liveness is
// faked per pid, 555555 being the one "live" foreign pid).
const RUN_A: &str = "20260101T000000_000000-1001"; // deploy, all confirmed
const RUN_B: &str = "20260101T000100_000000-1002"; // rollback + sticky confirmed
const RUN_C: &str = "20260101T000200_000000-1003"; // batch-build death
const RUN_D: &str = "20260101T000300_000000-1004"; // reboot, reaped rc=3
const RUN_E: &str = "20260101T000400_000000-1005"; // build, pid "alive"
const LIVE_PID: i64 = 555555;

/// The Python-written fixture state dir (checked in; see the README).
fn fixture_state() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cli/tests/fixtures/interop/state")
}

fn fixture_runs() -> PathBuf {
    fixture_state().join("runs")
}

/// A unique scratch dir for one test.
fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mandala-interop-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

/// Every Python-written `meta.json` survives a Rust read → write round-trip
/// byte-for-byte: the two writers share one format (`json.dumps(indent=1,
/// sort_keys=True)`), so a Rust rewrite (the reaper's `update_meta`) never
/// churns a Python-written file.
#[test]
fn python_meta_bytes_roundtrip_through_rust_writer() {
    let out = tmp_dir("meta-roundtrip");
    for run in [RUN_A, RUN_B, RUN_C, RUN_D, RUN_E] {
        let dir = fixture_runs().join(run);
        let original = std::fs::read(dir.join("meta.json")).unwrap();
        let meta = registry::read_meta(&dir);
        assert!(!meta.is_empty(), "fixture meta parsed empty for {run}");
        registry::write_meta(&out, &meta).unwrap();
        let rewritten = std::fs::read(out.join("meta.json")).unwrap();
        assert_eq!(
            rewritten, original,
            "meta.json byte round-trip diverged for {run}"
        );
    }
}

/// The Python-written `.expected.json` cache reads correctly and a Rust
/// `save_expected` of the same values reproduces the exact bytes.
#[test]
fn python_expected_cache_reads_and_rewrites_byte_identical() {
    let state = fixture_state();
    let (rev, toplevels) = drift::load_expected(&state);
    assert_eq!(
        rev.as_deref(),
        Some("0123456789abcdef0123456789abcdef01234567")
    );
    assert_eq!(toplevels.len(), 5);
    assert!(
        toplevels["alpha"].ends_with("-nixos-system-alpha-26.05"),
        "unexpected alpha toplevel: {}",
        toplevels["alpha"]
    );

    let out = tmp_dir("expected-rewrite");
    drift::save_expected(rev.as_deref(), &toplevels, &out).unwrap();
    assert_eq!(
        std::fs::read(out.join(".expected.json")).unwrap(),
        std::fs::read(state.join(".expected.json")).unwrap(),
        ".expected.json byte round-trip diverged"
    );
}

/// The drift judgement over the Python-written snapshots + cache: every
/// status in the vocabulary, including the kernel-params token
/// normalization (delta = activated, not reboot-pending) and the
/// `.expected.json` dot-file being excluded from the snapshot glob.
#[test]
fn python_snapshots_judge_identically() {
    let state = fixture_state();
    let snapshots = drift::read_snapshots(&state);
    assert!(
        !snapshots.contains_key(".expected"),
        "the cache dot-file must not read as a snapshot"
    );
    let (_, expected) = drift::load_expected(&state);
    let nodes: Vec<String> = [
        "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    let now = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
    let entries = drift::compare(
        &nodes,
        &snapshots,
        Some(&expected),
        Some(default_max_age()),
        now,
    );
    let got: BTreeMap<&str, DriftStatus> = entries
        .iter()
        .map(|e| (e.host.as_str(), e.status))
        .collect();
    let want: BTreeMap<&str, DriftStatus> = [
        ("alpha", DriftStatus::InSync),
        ("beta", DriftStatus::Drift),
        ("gamma", DriftStatus::RebootPending),
        ("delta", DriftStatus::Activated),
        ("epsilon", DriftStatus::Unreachable),
        ("zeta", DriftStatus::Incomplete),
        ("eta", DriftStatus::NoSnapshot),
        ("theta", DriftStatus::Stale),
    ]
    .into();
    assert_eq!(got, want);
}

/// A Python-written deploy run reads identically: milestones detected from
/// real deploy-rs lines, sticky confirmed states, per-host rc, the v2
/// nixlog stream reaching the sink, and the build model's counters —
/// while the torn final line in `beta.jsonl` is NOT consumed.
#[test]
fn python_written_deploy_run_reads_identically() {
    let _g = test_hooks::install_runs_base(fixture_runs());
    let mut obs = registry::open_run(RUN_A).expect("fixture run missing");
    assert_eq!(obs.info.kind(), "deploy");
    assert_eq!(
        obs.info.meta.get("limit").and_then(Value::as_str),
        Some("alpha,beta")
    );

    let nixlog: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&nixlog);
    obs.tailer.nixlog_sink = Some(Box::new(move |l| sink.lock().unwrap().push(l)));
    obs.poll();

    let alpha = &obs.tailer.hosts["alpha"];
    assert_eq!(alpha.state, HostState::Confirmed);
    assert_eq!(alpha.rc, Some(0));
    assert_eq!(
        alpha.milestones,
        vec!["eval", "build", "copy", "activate", "wait", "confirm"]
    );

    let beta = &obs.tailer.hosts["beta"];
    assert_eq!(beta.state, HostState::Confirmed);
    assert_eq!(beta.rc, Some(0));
    // The torn record ("tail-marker-line") must not surface.
    assert!(
        !beta.lines.iter().any(|l| l.contains("tail-marker-line")),
        "the torn final line was consumed"
    );

    let logs = nixlog.lock().unwrap();
    assert_eq!(logs.len(), 1);
    assert!(logs[0].starts_with("@nix "));

    let build = &obs.tailer.build;
    assert_eq!(
        (
            build.built,
            build.finished,
            build.fetched,
            build.fetched_done
        ),
        (3, 2, 5, 5)
    );
    assert_eq!(build.current, "nixos-system-alpha");
    assert!(build.done);
    assert_eq!(build.rc, Some(0));
}

/// A torn (partially written) final line is re-read once completed: poll
/// over a copy of the Python-written stream, append the cut-off remainder
/// (`beta.jsonl.tail`), and the record lands on the next poll.
#[test]
fn torn_line_completes_on_next_poll() {
    let dir = tmp_dir("torn");
    let src = fixture_runs().join(RUN_A);
    std::fs::copy(src.join("beta.jsonl"), dir.join("beta.jsonl")).unwrap();

    let mut tailer = EventTailer::new(&dir);
    tailer.poll();
    assert!(
        !tailer.hosts["beta"]
            .lines
            .iter()
            .any(|l| l.contains("tail-marker-line"))
    );

    // The writer finishes its interrupted write; the tailer resumes from
    // the un-advanced offset and consumes exactly the completed record.
    let tail = std::fs::read(src.join("beta.jsonl.tail")).unwrap();
    use std::io::Write as _;
    let mut fh = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join("beta.jsonl"))
        .unwrap();
    fh.write_all(&tail).unwrap();
    drop(fh);

    assert_eq!(tailer.poll(), 1);
    assert!(
        tailer.hosts["beta"]
            .lines
            .iter()
            .any(|l| l == "tail-marker-line")
    );
}

/// Cross-implementation liveness agreement: the same Python-written run
/// dirs yield the same verdicts — running while the pid lives, finished
/// from sticky terminal states, rollback-wins, batch-build-death=failed,
/// the reaped-rc path, and unknown for an unreaped dead command run. The
/// unsupported v99 record in RUN_B must be skipped with later records
/// (delta's `done rc=1`) still consumed.
#[test]
fn python_liveness_agreement() {
    let _base = test_hooks::install_runs_base(fixture_runs());

    let case = |run_id: &str, alive: bool, want: RunLiveness| {
        let _pid = test_hooks::install(move |_| alive);
        let mut obs = registry::open_run(run_id).expect("fixture run missing");
        obs.poll();
        assert_eq!(obs.liveness(), want, "run {run_id} (alive={alive})");
        obs
    };

    case(RUN_A, true, RunLiveness::Running);
    case(RUN_A, false, RunLiveness::Finished);
    let obs_b = case(RUN_B, false, RunLiveness::RolledBack);
    // Sticky confirmed + the post-v99 record consumed.
    assert_eq!(obs_b.tailer.hosts["delta"].state, HostState::Confirmed);
    assert_eq!(obs_b.tailer.hosts["delta"].rc, Some(1));
    assert!(
        !obs_b.tailer.hosts["delta"]
            .lines
            .iter()
            .any(|l| l.contains("future-protocol-noise")),
        "an unsupported-version record was consumed"
    );
    case(RUN_C, false, RunLiveness::Failed); // batch-build death
    let obs_d = case(RUN_D, false, RunLiveness::Failed); // reaped rc=3
    assert_eq!(obs_d.info.kind(), "reboot");
    case(RUN_E, true, RunLiveness::Running);
    case(RUN_E, false, RunLiveness::Unknown); // dead pid, no rc, no events
}

/// `DeployRun::attach` over Python-written runs: adopts the recorded limit,
/// and derives finished/returncode from the registry pid + sticky states.
#[test]
fn deploy_run_attach_over_python_written_runs() {
    let _base = test_hooks::install_runs_base(fixture_runs());
    let _pid = test_hooks::install(|_| false);

    let mut ok = DeployRun::attach(RUN_A).expect("fixture run missing");
    assert_eq!(ok.limit, "alpha,beta");
    ok.poll();
    assert!(ok.finished());
    assert_eq!(ok.returncode(), Some(0));

    let mut rb = DeployRun::attach(RUN_B).expect("fixture run missing");
    rb.poll();
    assert!(rb.finished());
    assert_eq!(rb.returncode(), Some(1), "a rolled-back host must fail");
}

/// Keep-N pruning spares the FOREIGN implementation's live run: over a copy
/// of the Python-written registry, `prune(keep=1)` keeps the live-pid run
/// (oldest runs otherwise pruned first) plus the most-recent dead run.
#[test]
fn rust_prune_spares_python_live_run() {
    let base = tmp_dir("prune");
    copy_tree(&fixture_runs(), &base);
    let _base = test_hooks::install_runs_base(base.clone());
    let _pid = test_hooks::install(|pid| pid == Some(LIVE_PID));

    registry::prune(Some(1));
    let survivors: Vec<String> = registry::list_runs()
        .into_iter()
        .map(|r| r.run_id)
        .collect();
    assert_eq!(survivors, vec![RUN_E.to_string(), RUN_D.to_string()]);
}

/// Python-written run ids list most-recent first (the id format sorts
/// lexically by start time — shared by both implementations' `now_id`).
#[test]
fn python_run_ids_sort_most_recent_first() {
    let _base = test_hooks::install_runs_base(fixture_runs());
    let ids: Vec<String> = registry::list_runs()
        .into_iter()
        .map(|r| r.run_id)
        .collect();
    assert_eq!(ids, vec![RUN_E, RUN_D, RUN_C, RUN_B, RUN_A]);
}
