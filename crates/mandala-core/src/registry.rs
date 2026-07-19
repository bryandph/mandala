//! Discoverable deploy-run registry: a per-user directory of recent runs so any
//! frontend — a second TUI, the CLI, the fleet MCP server — can find an
//! in-flight or recent run and tail its event streams.
//!
//! A parity port of the retired Python `mandala_fleet.registry`. Each run owns a
//! directory under `state_dir()/runs/<run-id>/` holding its per-host event
//! JSONLs (what [`EventTailer`] globs) plus a small `meta.json` (limit, pid,
//! kind, started_at, rc, …). Reusing [`crate::drift::state_dir`] keeps one
//! per-user state root, and the run-id sorts lexically by start time so listing
//! is a sorted glob.
//!
//! Everything an observer does here is read-only: it opens an existing run dir,
//! tails its files, and derives liveness from the recorded pid plus the
//! protocol's sticky terminal host states. It never owns the subprocess.
//!
//! ## Cross-implementation formats (fleet-state-formats)
//!
//! `meta.json` is a byte-level compatibility contract: written atomically
//! (write `meta.json.tmp`, then rename) as the Python `json.dumps(indent=1,
//! sort_keys=True)` bytes — the same 1-space sorted format as `.expected.json`,
//! reusing [`crate::drift`]'s formatter so a run this writes is read identically
//! by the Python `registry.read_meta` and vice versa. serde_json's `Map` is
//! `BTreeMap`-backed here (no `preserve_order`), so keys serialize sorted for
//! free.

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::drift::{state_dir, to_pretty_1space};
use crate::runner::{EventTailer, HostState, is_terminal};

/// Keep the N most-recent run dirs; older ones are pruned when a new run is
/// allocated. A run whose recorded pid is still alive is NEVER pruned. Override
/// via `MANDALA_FLEET_RUN_KEEP`.
pub const DEFAULT_KEEP: usize = 20;

const META: &str = "meta.json";
const META_TMP: &str = "meta.json.tmp";

/// A run's `meta.json` payload — a JSON object. `serde_json::Map` is
/// `BTreeMap`-backed (no `preserve_order` feature), so it iterates key-sorted,
/// giving `sort_keys=True` for free on write.
pub type Meta = serde_json::Map<String, Value>;

/// The run-registry root, resolved at call time (mirrors [`state_dir`]).
#[must_use]
pub fn runs_dir() -> PathBuf {
    // Tests point the runners' `new_run_dir()` at a private tmp base via a
    // thread-local override (the runner's write-side tests spawn subprocesses
    // that go through `new_run_dir()` — which reads `state_dir()` — so this is
    // the env-free, race-free seam, mirroring the `pid_alive` hook below).
    #[cfg(test)]
    if let Some(base) = test_hooks::runs_base() {
        return base;
    }
    state_dir().join("runs")
}

/// The retention cap: `MANDALA_FLEET_RUN_KEEP` (clamped to at least 1) else
/// [`DEFAULT_KEEP`]. An empty or unparseable value falls back to the default.
fn keep_env() -> usize {
    if let Ok(raw) = std::env::var("MANDALA_FLEET_RUN_KEEP")
        && !raw.is_empty()
        && let Ok(n) = raw.parse::<i64>()
    {
        return n.max(1) as usize;
    }
    DEFAULT_KEEP
}

/// A fresh run id: `%Y%m%dT%H%M%S_%6f-<pid>`. Lexically sortable by start time;
/// microseconds + pid disambiguate two runs launched in the same second.
fn now_id() -> String {
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S_%6f");
    format!("{ts}-{}", std::process::id())
}

/// Whether `run_id` has Mandala's generated `%Y%m%dT%H%M%S_%6f-<pid>`
/// shape. Registry attachment treats the id as untrusted input: accepting
/// only this basename grammar prevents absolute-path replacement and `..`
/// traversal when it is joined beneath [`runs_dir`].
#[must_use]
pub fn is_valid_run_id(run_id: &str) -> bool {
    let bytes = run_id.as_bytes();
    if bytes.len() < 24
        || bytes.get(8) != Some(&b'T')
        || bytes.get(15) != Some(&b'_')
        || bytes.get(22) != Some(&b'-')
    {
        return false;
    }
    let timestamp_digits = bytes[..8].iter().chain(&bytes[9..15]).chain(&bytes[16..22]);
    timestamp_digits.cloned().all(|b| b.is_ascii_digit())
        && chrono::NaiveDateTime::parse_from_str(&run_id[..22], "%Y%m%dT%H%M%S_%6f").is_ok()
        && bytes[23..].iter().all(u8::is_ascii_digit)
        && bytes[23..]
            .iter()
            .fold(0_u64, |n, b| n.saturating_mul(10) + u64::from(b - b'0'))
            > 0
}

/// Whether a recorded run pid is still running. Signal 0 probes existence
/// without delivering anything: `ESRCH` → gone, `EPERM` → exists (owned by
/// another user). A `None`/zero pid is never alive.
///
/// Caveat (recycled pid): the OS can reuse a pid, so a long-dead run whose pid
/// was recycled reads as alive. The Python porcelain carries the same caveat;
/// cross-check argv/proc start time if it ever bites (see `mem:mandala/mcp`).
#[must_use]
pub fn pid_alive(pid: Option<i64>) -> bool {
    #[cfg(test)]
    if let Some(v) = test_hooks::with(pid) {
        return v;
    }
    real_pid_alive(pid)
}

fn real_pid_alive(pid: Option<i64>) -> bool {
    let pid = match pid {
        Some(p) if p != 0 => p,
        _ => return false,
    };
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    match kill(Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(Errno::EPERM) => true, // exists, owned by another user
        Err(_) => false,           // ESRCH (gone) or any other error
    }
}

/// The whole-run liveness verdict, derived from the recorded pid, then the
/// reaped rc, then the sticky-terminal host states. Port of the Python
/// `RunLiveness` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RunLiveness {
    /// Recorded pid alive, no whole-run terminal yet.
    #[serde(rename = "running")]
    Running,
    /// Pid gone, every host terminal, none failed (or a reaped rc == 0).
    #[serde(rename = "finished")]
    Finished,
    /// Pid gone, a host failed (or a reaped rc != 0, or a batch-build death).
    #[serde(rename = "failed")]
    Failed,
    /// Pid gone, a host rolled back.
    #[serde(rename = "rolled-back")]
    RolledBack,
    /// Pid gone, no terminal state reached.
    #[serde(rename = "unknown")]
    Unknown,
}

impl RunLiveness {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RunLiveness::Running => "running",
            RunLiveness::Finished => "finished",
            RunLiveness::Failed => "failed",
            RunLiveness::RolledBack => "rolled-back",
            RunLiveness::Unknown => "unknown",
        }
    }
}

/// A registered run's identity + its `meta.json`. Port of the Python `RunInfo`.
#[derive(Debug, Clone)]
pub struct RunInfo {
    pub run_id: String,
    pub path: PathBuf,
    pub meta: Meta,
}

impl RunInfo {
    /// The recorded pid (absent, null, or non-integer → `None`).
    #[must_use]
    pub fn pid(&self) -> Option<i64> {
        self.meta.get("pid").and_then(Value::as_i64)
    }

    /// What launched into this run dir: `deploy` (event-streaming playbook, the
    /// default) or a command kind (`reboot`, …) whose only stream is its teed
    /// `output.log`.
    #[must_use]
    pub fn kind(&self) -> &str {
        self.meta
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("deploy")
    }
}

/// Read a run dir's `meta.json` (`{}` on any error / non-object payload).
#[must_use]
pub fn read_meta(path: &Path) -> Meta {
    let Ok(text) = std::fs::read_to_string(path.join(META)) else {
        return Meta::new();
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m)) => m,
        _ => Meta::new(),
    }
}

/// Atomically write a run dir's `meta.json` (write the tmp file, then rename)
/// as the Python `json.dumps(indent=1, sort_keys=True)` bytes. The launcher
/// REWRITES meta (the reaper recording rc) while observers poll it, so a
/// partial read must never surface as an empty meta / wrong kind.
///
/// # Errors
/// Any filesystem or serialization error.
pub fn write_meta(path: &Path, meta: &Meta) -> io::Result<()> {
    let bytes = to_pretty_1space(meta).map_err(io::Error::other)?;
    let tmp = path.join(META_TMP);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path.join(META))
}

/// Merge fields into an existing `meta.json` (read-modify-write) — how a
/// launcher records the exit code once its command run finishes. Later fields
/// win.
///
/// # Errors
/// Any filesystem or serialization error.
pub fn update_meta(path: &Path, fields: Meta) -> io::Result<()> {
    let mut meta = read_meta(path);
    for (k, v) in fields {
        meta.insert(k, v);
    }
    write_meta(path, &meta)
}

/// Recent runs, most-recent first (the run-id sorts by start time).
#[must_use]
pub fn list_runs() -> Vec<RunInfo> {
    list_runs_in(&runs_dir())
}

fn list_runs_in(base: &Path) -> Vec<RunInfo> {
    let Ok(rd) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    let mut runs: Vec<RunInfo> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter_map(|p| {
            let run_id = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            if !is_valid_run_id(&run_id) {
                return None;
            }
            let meta = read_meta(&p);
            Some(RunInfo {
                run_id,
                path: p,
                meta,
            })
        })
        .collect();
    // Reverse-sort by run-id: later id == more recent.
    runs.sort_by(|a, b| b.run_id.cmp(&a.run_id));
    runs
}

/// Drop all but the most-recent `keep` run dirs; never drop a run whose
/// recorded pid is still alive (an observer may be attached, and a live run
/// does not count against the cap). `keep = None` reads the env cap.
pub fn prune(keep: Option<usize>) {
    prune_in(&runs_dir(), keep);
}

fn prune_in(base: &Path, keep: Option<usize>) {
    let keep = keep.unwrap_or_else(keep_env);
    let mut survivors = 0usize;
    for info in list_runs_in(base) {
        // most-recent first
        if pid_alive(info.pid()) {
            continue; // live runs are kept and don't count against the cap
        }
        survivors += 1;
        if survivors > keep {
            let _ = std::fs::remove_dir_all(&info.path);
        }
    }
}

/// Prune stale runs, then allocate a fresh registered run directory.
///
/// # Errors
/// Any filesystem error creating the registry root or the run directory.
pub fn new_run_dir() -> io::Result<(String, PathBuf)> {
    new_run_dir_in(&runs_dir())
}

fn new_run_dir_in(base: &Path) -> io::Result<(String, PathBuf)> {
    prune_in(base, None);
    std::fs::create_dir_all(base)?;
    // Timestamp run-ids can COLLIDE when two threads of one process allocate
    // within the same microsecond (the pid suffix disambiguates processes,
    // not threads): both would then share — and meta-clobber / tmp-rename-
    // race — a single run dir. Claim with the exclusive `create_dir` and
    // retry until the clock advances; the id format stays byte-identical
    // (fleet-state-formats), only uniqueness is enforced.
    loop {
        let run_id = now_id();
        let path = base.join(&run_id);
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok((run_id, path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Read-only attachment to an existing run dir: tail its events and judge
/// liveness without owning the subprocess. Port of the Python `ObservedRun`.
pub struct ObservedRun {
    pub info: RunInfo,
    pub tailer: EventTailer,
}

impl ObservedRun {
    /// Consume newly appended events; returns how many were read.
    pub fn poll(&mut self) -> usize {
        self.tailer.poll()
    }

    /// The whole-run liveness verdict.
    ///
    /// Meta is re-read each call (the launcher updates it — pid at start, rc
    /// when the reaper fires) so a long-attached observer sees the exit code
    /// land instead of judging from a stale snapshot. The decision tree: pid
    /// alive → running; else a reaped rc → finished/failed; else host states →
    /// rolled-back / failed / finished (all terminal); else a batch-build death
    /// (build done, rc ∉ {0, None}, no host events) → failed; else unknown.
    pub fn liveness(&mut self) -> RunLiveness {
        self.info.meta = read_meta(&self.info.path);
        let supervisor = self.info.meta.get("supervisor_pid").and_then(Value::as_i64);
        if self.info.meta.get("rc").is_none() && supervisor.is_some() && !pid_alive(supervisor) {
            return RunLiveness::Failed;
        }
        // A live pid means the fan-out is still going, even if one host has
        // already reached a sticky terminal state.
        if pid_alive(self.info.pid()) {
            return RunLiveness::Running;
        }
        // A command run (reboot, …) has no host event streams; its launcher
        // records the exit code into meta when the subprocess exits.
        if let Some(rc) = self.info.meta.get("rc").and_then(Value::as_i64) {
            return if rc == 0 {
                RunLiveness::Finished
            } else {
                RunLiveness::Failed
            };
        }
        let states: Vec<HostState> = self.tailer.hosts.values().map(|h| h.state).collect();
        if states.contains(&HostState::RolledBack) {
            return RunLiveness::RolledBack;
        }
        if states.contains(&HostState::Failed) {
            return RunLiveness::Failed;
        }
        if !states.is_empty() && states.iter().all(|s| is_terminal(*s)) {
            return RunLiveness::Finished;
        }
        // A deploy that died in the batch build never emitted host events —
        // judge from the build stream so it lands failed, not unknown.
        let build = &self.tailer.build;
        if build.done && !matches!(build.rc, Some(0) | None) {
            return RunLiveness::Failed;
        }
        RunLiveness::Unknown
    }
}

/// Attach read-only to a registered run by id (`None` if it's gone).
#[must_use]
pub fn open_run(run_id: &str) -> Option<ObservedRun> {
    open_run_in(&runs_dir(), run_id)
}

fn open_run_in(base: &Path, run_id: &str) -> Option<ObservedRun> {
    if !is_valid_run_id(run_id) {
        return None;
    }
    let path = base.join(run_id);
    if !path.is_dir() {
        return None;
    }
    let info = RunInfo {
        run_id: run_id.to_string(),
        path: path.clone(),
        meta: read_meta(&path),
    };
    Some(ObservedRun {
        info,
        tailer: EventTailer::new(&path),
    })
}

/// A test-only monkeypatch seam for [`pid_alive`], mirroring the Python tests'
/// `monkeypatch.setattr(registry, "pid_alive", …)`. Thread-local so parallel
/// tests never race; a [`test_hooks::Guard`] clears it on drop. `pub(crate)`
/// so the runner's tests can fake liveness for `DeployRun::attach` too.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::cell::RefCell;
    use std::path::PathBuf;

    type Hook = Box<dyn Fn(Option<i64>) -> bool>;

    thread_local! {
        static PID_ALIVE: RefCell<Option<Hook>> = const { RefCell::new(None) };
        static RUNS_BASE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    /// If a hook is installed on this thread, evaluate it.
    pub fn with(pid: Option<i64>) -> Option<bool> {
        PID_ALIVE.with(|h| h.borrow().as_ref().map(|f| f(pid)))
    }

    /// A drop-guard that clears the installed hook.
    pub struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            PID_ALIVE.with(|h| *h.borrow_mut() = None);
        }
    }

    /// Install a pid-liveness hook for the current thread; returns a guard.
    pub fn install(f: impl Fn(Option<i64>) -> bool + 'static) -> Guard {
        PID_ALIVE.with(|h| *h.borrow_mut() = Some(Box::new(f)));
        Guard
    }

    /// The thread-local run-registry root override, if any (consulted by
    /// [`super::runs_dir`]).
    pub fn runs_base() -> Option<PathBuf> {
        RUNS_BASE.with(|b| b.borrow().clone())
    }

    /// A drop-guard clearing the run-base override.
    pub struct RunsBaseGuard;
    impl Drop for RunsBaseGuard {
        fn drop(&mut self) {
            RUNS_BASE.with(|b| *b.borrow_mut() = None);
        }
    }

    /// Point `new_run_dir()` at `base` for the current thread; returns a guard.
    pub fn install_runs_base(base: PathBuf) -> RunsBaseGuard {
        RUNS_BASE.with(|b| *b.borrow_mut() = Some(base));
        RunsBaseGuard
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp() -> PathBuf {
        // A process-wide counter, not just (pid, now): the sandbox clock is
        // coarse enough that two parallel tests can read the same nanos and
        // silently SHARE a scratch dir (one test's real-timestamped run then
        // pollutes another's listing).
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mandala-registry-test-{}-{}-{:?}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn meta(pairs: &[(&str, Value)]) -> Meta {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    fn write_events(path: &Path, events: &[Value]) {
        use std::io::Write;
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
            .map(|n| serde_json::json!({"host":host,"plugin":"deploy","event":"milestone","milestone":n}))
            .collect()
    }

    // ---- meta round-trip + list ordering ------------------------------------

    /// Parallel same-process allocations must never share a run dir: the
    /// timestamp id's pid suffix disambiguates processes, not threads, so a
    /// same-microsecond tie would meta-clobber — the exclusive-create claim
    /// loop in `new_run_dir_in` is the guard (the flake that failed the 7.1
    /// packaging gate: two TUI tests raced `write_meta` in one shared dir).
    #[test]
    fn parallel_new_run_dirs_are_unique() {
        let base = std::sync::Arc::new(tmp().join("runs"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let base = std::sync::Arc::clone(&base);
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait(); // maximize same-instant contention
                    new_run_dir_in(&base).unwrap().0
                })
            })
            .collect();
        let mut ids: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 8, "every allocation must claim a unique dir");
    }

    #[test]
    fn new_run_dir_and_meta_roundtrip() {
        let base = tmp().join("runs");
        let (run_id, path) = new_run_dir_in(&base).unwrap();
        assert!(path.is_dir());
        assert_eq!(path, base.join(&run_id));
        write_meta(
            &path,
            &meta(&[
                ("run_id", Value::from(run_id.clone())),
                ("limit", Value::from("@all")),
                ("pid", Value::from(1234)),
            ]),
        )
        .unwrap();
        assert_eq!(read_meta(&path)["limit"], Value::from("@all"));
        let runs = list_runs_in(&base);
        assert_eq!(
            runs.iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(),
            vec![run_id.as_str()]
        );
        assert_eq!(runs[0].pid(), Some(1234));
    }

    #[test]
    fn run_id_validation_accepts_generated_ids_only() {
        assert!(is_valid_run_id("20260719T012345_123456-42"));
        for invalid in [
            "",
            "nonesuch",
            "/tmp/20260719T012345_123456-42",
            "../20260719T012345_123456-42",
            "20260719T012345_123456-0",
            "20261319T012345_123456-42",
            "20260719T012345_12345x-42",
            "20260719T012345_123456-42/child",
        ] {
            assert!(!is_valid_run_id(invalid), "accepted {invalid:?}");
        }
    }

    #[test]
    fn open_run_rejects_paths_outside_registry() {
        let base = tmp().join("runs");
        std::fs::create_dir_all(&base).unwrap();
        let outside = tmp().join("20260719T012345_123456-42");
        std::fs::create_dir_all(&outside).unwrap();
        assert!(open_run_in(&base, outside.to_str().unwrap()).is_none());
        assert!(open_run_in(&base, "../20260719T012345_123456-42").is_none());
    }

    #[test]
    fn list_runs_sorts_most_recent_first() {
        let base = tmp().join("runs");
        std::fs::create_dir_all(&base).unwrap();
        // run-ids sort lexically by start time; later id == more recent.
        for rid in ["20260101T000000_000000-1", "20260102T000000_000000-1"] {
            std::fs::create_dir_all(base.join(rid)).unwrap();
        }
        assert_eq!(
            list_runs_in(&base)
                .iter()
                .map(|r| r.run_id.as_str())
                .collect::<Vec<_>>(),
            vec!["20260102T000000_000000-1", "20260101T000000_000000-1"]
        );
    }

    // ---- pruning: keep-N, spare-live, live-doesn't-count ---------------------

    #[test]
    fn prune_keeps_recent_and_spares_live_pids() {
        let base = tmp().join("runs");
        std::fs::create_dir_all(&base).unwrap();
        // Six dead runs + one old live run; keep=2.
        let ids: Vec<String> = (0..6)
            .map(|n| format!("20260101T0000{n:02}_000000-1"))
            .collect();
        for rid in &ids {
            let d = base.join(rid);
            std::fs::create_dir_all(&d).unwrap();
            write_meta(&d, &meta(&[("pid", Value::from(999_999))])).unwrap(); // dead
        }
        let live = base.join("20260101T000000_000000-9"); // oldest by id
        std::fs::create_dir_all(&live).unwrap();
        write_meta(&live, &meta(&[("pid", Value::from(4242))])).unwrap(); // "alive"

        let _g = test_hooks::install(|pid| pid == Some(4242));
        prune_in(&base, Some(2));

        let survivors: std::collections::BTreeSet<String> =
            list_runs_in(&base).into_iter().map(|r| r.run_id).collect();
        // The two most-recent dead runs survive, plus the live one regardless of age.
        assert!(survivors.contains(&ids[5]) && survivors.contains(&ids[4]));
        assert!(!survivors.contains(&ids[0]) && !survivors.contains(&ids[3]));
        assert!(survivors.contains("20260101T000000_000000-9"));
    }

    // ---- liveness decision tree ---------------------------------------------

    #[test]
    fn open_run_liveness_running_then_terminal() {
        let base = tmp().join("runs");
        let (run_id, path) = new_run_dir_in(&base).unwrap();
        write_meta(
            &path,
            &meta(&[
                ("run_id", Value::from(run_id.clone())),
                ("pid", Value::from(4242)),
            ]),
        )
        .unwrap();
        write_events(
            &path.join("alpha.jsonl"),
            &milestones("alpha", &["eval", "build", "copy", "activate", "confirm"]),
        );

        let mut obs = open_run_in(&base, &run_id).unwrap();
        obs.poll();
        // pid alive → RUNNING even though alpha already confirmed.
        let g = test_hooks::install(|_| true);
        assert_eq!(obs.liveness(), RunLiveness::Running);
        drop(g);
        // pid gone, all hosts terminal & confirmed → FINISHED.
        let _g = test_hooks::install(|_| false);
        assert_eq!(obs.liveness(), RunLiveness::Finished);
    }

    #[test]
    fn open_run_liveness_rollback_and_unknown() {
        let base = tmp().join("runs");
        let _g = test_hooks::install(|_| false);

        let (rb_id, rb) = new_run_dir_in(&base).unwrap();
        write_meta(&rb, &meta(&[("pid", Value::from(1))])).unwrap();
        write_events(
            &rb.join("beta.jsonl"),
            &milestones("beta", &["eval", "activate", "rollback"]),
        );
        let mut obs_rb = open_run_in(&base, &rb_id).unwrap();
        obs_rb.poll();
        assert_eq!(obs_rb.liveness(), RunLiveness::RolledBack);

        // Dead pid, host stuck mid-flight (no terminal state) → UNKNOWN.
        let (unk_id, unk) = new_run_dir_in(&base).unwrap();
        write_meta(&unk, &meta(&[("pid", Value::from(1))])).unwrap();
        write_events(
            &unk.join("gamma.jsonl"),
            &milestones("gamma", &["eval", "copy"]),
        );
        let mut obs_unk = open_run_in(&base, &unk_id).unwrap();
        obs_unk.poll();
        assert_eq!(obs_unk.liveness(), RunLiveness::Unknown);

        assert!(open_run_in(&base, "nonesuch").is_none());
    }

    #[test]
    fn command_run_liveness_from_reaped_rc() {
        // A command run (reboot) has no host event streams: liveness comes from
        // the pid, then the exit code the launcher's reaper recorded — and a
        // long-attached observer must see the rc land (meta re-read).
        let base = tmp().join("runs");
        let (run_id, path) = new_run_dir_in(&base).unwrap();
        write_meta(
            &path,
            &meta(&[
                ("run_id", Value::from(run_id.clone())),
                ("kind", Value::from("reboot")),
                ("pid", Value::from(4242)),
            ]),
        )
        .unwrap();
        let mut obs = open_run_in(&base, &run_id).unwrap();
        assert_eq!(obs.info.kind(), "reboot");

        let g = test_hooks::install(|pid| pid == Some(4242));
        assert_eq!(obs.liveness(), RunLiveness::Running);
        drop(g);

        // The reaper fires AFTER the observer attached: pid replaced, rc merged.
        let _g = test_hooks::install(|_| false);
        update_meta(&path, meta(&[("pid", Value::Null), ("rc", Value::from(0))])).unwrap();
        assert_eq!(obs.liveness(), RunLiveness::Finished);

        update_meta(&path, meta(&[("rc", Value::from(2))])).unwrap();
        assert_eq!(obs.liveness(), RunLiveness::Failed);
    }

    #[test]
    fn dead_unsettled_supervisor_is_failed_even_if_child_pid_lingers() {
        let base = tmp().join("runs");
        let (run_id, path) = new_run_dir_in(&base).unwrap();
        write_meta(
            &path,
            &meta(&[
                ("run_id", Value::from(run_id.clone())),
                ("pid", Value::from(2222)),
                ("supervisor_pid", Value::from(1111)),
            ]),
        )
        .unwrap();
        let _guard = test_hooks::install(|pid| pid == Some(2222));
        let mut observed = open_run_in(&base, &run_id).unwrap();
        assert_eq!(observed.liveness(), RunLiveness::Failed);
    }

    /// The batch-build-death path: a deploy that died in the batch build
    /// emitted no host events, so liveness must fall to the build stream and
    /// land FAILED (not UNKNOWN).
    #[test]
    fn liveness_batch_build_death_is_failed() {
        let base = tmp().join("runs");
        let (run_id, path) = new_run_dir_in(&base).unwrap();
        write_meta(&path, &meta(&[("pid", Value::from(1))])).unwrap();
        write_events(
            &path.join("alpha.jsonl"),
            &[
                serde_json::json!({"host":"alpha","plugin":"build","event":"status","state":"start","cmd":[]}),
                serde_json::json!({"host":"alpha","plugin":"build","event":"status","state":"done","rc":1}),
            ],
        );
        let mut obs = open_run_in(&base, &run_id).unwrap();
        obs.poll();
        assert!(obs.tailer.hosts.is_empty()); // no host events, only build
        let _g = test_hooks::install(|_| false);
        assert_eq!(obs.liveness(), RunLiveness::Failed);
    }

    // ---- pid_alive (real kill(0) semantics) ---------------------------------

    #[test]
    fn pid_alive_probes_existence() {
        // Our own pid is alive; a None/zero pid never is.
        assert!(real_pid_alive(Some(i64::from(std::process::id()))));
        assert!(!real_pid_alive(None));
        assert!(!real_pid_alive(Some(0)));
    }

    // ---- meta.json byte-format interop --------------------------------------

    /// `meta.json` is a cross-implementation byte contract: assert this
    /// writer's bytes match Python `json.dumps(indent=1, sort_keys=True)`
    /// exactly (one-space indent, sorted keys, no trailing newline). Ground
    /// truth captured from CPython.
    #[test]
    fn meta_byte_format_matches_python() {
        let base = tmp().join("runs");
        let (_run_id, path) = new_run_dir_in(&base).unwrap();
        write_meta(
            &path,
            &meta(&[
                ("limit", Value::from("web,cache")),
                ("pid", Value::from(4242)),
                ("kind", Value::from("deploy")),
            ]),
        )
        .unwrap();
        let bytes = std::fs::read_to_string(path.join(META)).unwrap();
        // python3 -c 'import json; print(json.dumps({"limit":"web,cache",
        //   "pid":4242,"kind":"deploy"}, indent=1, sort_keys=True))'
        let expected = "{\n \"kind\": \"deploy\",\n \"limit\": \"web,cache\",\n \"pid\": 4242\n}";
        assert_eq!(bytes, expected);
    }

    /// The reverse direction: a Python-written `meta.json` (embedded bytes,
    /// including `rc: null`) is read back and its typed accessors behave — the
    /// cross-implementation read path.
    #[test]
    fn read_meta_reads_python_bytes_and_accessors() {
        let base = tmp().join("runs");
        let (_run_id, path) = new_run_dir_in(&base).unwrap();
        // json.dumps({"kind":"reboot","pid":None,"rc":3,"limit":"web"},
        //   indent=1, sort_keys=True)
        let python_bytes =
            "{\n \"kind\": \"reboot\",\n \"limit\": \"web\",\n \"pid\": null,\n \"rc\": 3\n}";
        std::fs::write(path.join(META), python_bytes).unwrap();
        let info = RunInfo {
            run_id: "x".into(),
            path: path.clone(),
            meta: read_meta(&path),
        };
        assert_eq!(info.kind(), "reboot");
        assert_eq!(info.pid(), None); // null pid → None
        assert_eq!(info.meta.get("rc").and_then(Value::as_i64), Some(3));
    }

    /// A Python-written event JSONL (embedded bytes, exact Python
    /// `json.dumps` output) is tailed by the Rust reader into the identical
    /// model — the v1/v2 event protocol read-interop round-trip.
    #[test]
    fn tailer_reads_python_written_event_jsonl() {
        let base = tmp().join("runs");
        let (run_id, path) = new_run_dir_in(&base).unwrap();
        write_meta(&path, &meta(&[("pid", Value::from(1))])).unwrap();
        // Exact bytes CPython's json.dumps emits for these events, one per line
        // (compact default separators), as the fleet plugins append them.
        let python_jsonl = concat!(
            r#"{"v": 1, "ts": 0.0, "host": "alpha", "plugin": "build", "event": "progress", "built": 2, "finished": 2, "fetched": 0, "fetched_done": 0, "errors": 0, "current": "system-path"}"#,
            "\n",
            r#"{"v": 1, "ts": 0.0, "host": "alpha", "plugin": "build", "event": "status", "state": "done", "rc": 0}"#,
            "\n",
            r#"{"v": 1, "ts": 0.0, "host": "alpha", "plugin": "deploy", "event": "milestone", "milestone": "eval"}"#,
            "\n",
            r#"{"v": 1, "ts": 0.0, "host": "alpha", "plugin": "deploy", "event": "milestone", "milestone": "confirm"}"#,
            "\n",
            r#"{"v": 2, "host": "alpha", "plugin": "build", "event": "nixlog", "line": "@nix {}"}"#,
            "\n",
        );
        std::fs::write(path.join("alpha.jsonl"), python_jsonl).unwrap();

        let mut obs = open_run_in(&base, &run_id).unwrap();
        let n = obs.poll();
        assert_eq!(n, 5);
        assert!(obs.tailer.build.done && obs.tailer.build.rc == Some(0));
        assert_eq!(obs.tailer.build.built, 2);
        assert_eq!(obs.tailer.hosts["alpha"].state, HostState::Confirmed);
        let _g = test_hooks::install(|_| false);
        assert_eq!(obs.liveness(), RunLiveness::Finished);
    }
}
