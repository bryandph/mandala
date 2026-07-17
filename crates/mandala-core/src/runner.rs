//! Deploy-runner read model: demux the per-host JSONL event streams a run
//! writes into its registry dir, tailing them incrementally into host state
//! machines and a build-progress model.
//!
//! A parity port of the READ half of `cli/src/mandala_fleet/runner.py`
//! (`HostState`/`HostRun`, `BuildModel`, `EventTailer`). This is what any
//! frontend — a second TUI, the CLI, the fleet MCP server — uses to render an
//! in-flight or finished run from the shared event files, without owning the
//! subprocess that produced them.
//!
//! The event JSONL protocol (versions 1 and 2, gated by the `v` field) is the
//! whole cross-implementation contract: the Python porcelain writes it, this
//! reads it byte-compatibly, and vice versa. Unknown fields are tolerated (real
//! files carry more), unsupported versions are skipped rather than misread, and
//! a partial trailing line (a write in flight) is re-read on the next poll.
//!
//! The WRITE half (`DeployRun`/`CommandRun` — subprocess spawn, reader/reaper
//! tasks) lands in a later task; nothing here spawns a process.

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

/// Event protocol versions this reader understands. v2 = v1 + the `nixlog`
/// event type (verbatim internal-json for nom). A record with any other `v`
/// (or none) is dropped, not misread.
pub const SUPPORTED_EVENT_VERSIONS: [i64; 2] = [1, 2];

/// Whether an event's `v` field names a supported protocol version.
fn version_supported(v: Option<i64>) -> bool {
    matches!(v, Some(v) if SUPPORTED_EVENT_VERSIONS.contains(&v))
}

/// Raw lines kept per host for the inspector view (`_MAX_LINES`).
const MAX_LINES: usize = 2000;
/// Lines kept in the build pane.
const BUILD_MAX_LINES: usize = 200;

/// Per-host deploy state. The string values are the protocol contract (note
/// `rolled-back`, not `rolled_back`) — the MCP/TUI surface them verbatim — so
/// they are pinned via serde `rename` and [`HostState::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HostState {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "evaluating")]
    Evaluating,
    #[serde(rename = "building")]
    Building,
    #[serde(rename = "copying")]
    Copying,
    #[serde(rename = "activating")]
    Activating,
    #[serde(rename = "waiting")]
    Waiting,
    #[serde(rename = "confirmed")]
    Confirmed,
    #[serde(rename = "rolled-back")]
    RolledBack,
    #[serde(rename = "failed")]
    Failed,
}

impl HostState {
    /// The stable protocol string value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            HostState::Pending => "pending",
            HostState::Evaluating => "evaluating",
            HostState::Building => "building",
            HostState::Copying => "copying",
            HostState::Activating => "activating",
            HostState::Waiting => "waiting",
            HostState::Confirmed => "confirmed",
            HostState::RolledBack => "rolled-back",
            HostState::Failed => "failed",
        }
    }
}

impl std::fmt::Display for HostState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The milestone → state map (`_MILESTONE_STATE`). A milestone name not in the
/// map (e.g. an unknown future milestone) leaves the state untouched.
fn milestone_state(name: &str) -> Option<HostState> {
    match name {
        "eval" => Some(HostState::Evaluating),
        "build" => Some(HostState::Building),
        "copy" => Some(HostState::Copying),
        "activate" => Some(HostState::Activating),
        "wait" => Some(HostState::Waiting),
        "confirm" => Some(HostState::Confirmed),
        "rollback" => Some(HostState::RolledBack),
        _ => None,
    }
}

/// Terminal states are STICKY (`_TERMINAL`): a late `done rc=1` must not unflag
/// a rollback, and a confirmed host stays confirmed. Rollback is the one
/// exception — it wins even over confirmed (see [`HostRun::feed`]).
#[must_use]
pub fn is_terminal(state: HostState) -> bool {
    matches!(
        state,
        HostState::Confirmed | HostState::RolledBack | HostState::Failed
    )
}

/// Append to a capacity-bounded deque, dropping the oldest when full (the
/// parity of Python `deque(maxlen=…)`).
fn push_capped(dq: &mut VecDeque<String>, s: String, cap: usize) {
    if dq.len() >= cap {
        dq.pop_front();
    }
    dq.push_back(s);
}

/// One host's demuxed deploy state, fed from its `milestone`/`line`/`status`
/// events. Port of the Python `HostRun` dataclass.
#[derive(Debug, Clone)]
pub struct HostRun {
    pub name: String,
    pub state: HostState,
    pub lines: VecDeque<String>,
    pub milestones: Vec<String>,
    pub rc: Option<i64>,
}

impl HostRun {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        HostRun {
            name: name.into(),
            state: HostState::Pending,
            lines: VecDeque::new(),
            milestones: Vec::new(),
            rc: None,
        }
    }

    /// Feed one already-parsed event object into the state machine.
    ///
    /// * `line` → appended to the bounded line buffer.
    /// * `milestone` → set the state via [`milestone_state`] unless already
    ///   terminal, BUT a `rollback` milestone wins even over `confirmed`
    ///   (deploy-rs can confirm and then roll back on the magic-rollback
    ///   timeout).
    /// * `status` with `state == "done"` → record `rc`; a non-zero/non-null rc
    ///   flags [`HostState::Failed`] unless a terminal state already stuck.
    pub fn feed(&mut self, event: &Value) {
        match event.get("event").and_then(Value::as_str) {
            Some("line") => {
                let line = event.get("line").and_then(Value::as_str).unwrap_or("");
                push_capped(&mut self.lines, line.to_string(), MAX_LINES);
            }
            Some("milestone") => {
                let name = event.get("milestone").and_then(Value::as_str).unwrap_or("");
                self.milestones.push(name.to_string());
                match milestone_state(name) {
                    Some(state) if !is_terminal(self.state) => self.state = state,
                    // Rollback wins even over a terminal (confirmed) state.
                    Some(HostState::RolledBack) => self.state = HostState::RolledBack,
                    _ => {}
                }
            }
            Some("status") if event.get("state").and_then(Value::as_str) == Some("done") => {
                self.rc = event.get("rc").and_then(Value::as_i64);
                if !matches!(self.rc, Some(0) | None) && !is_terminal(self.state) {
                    self.state = HostState::Failed;
                }
            }
            _ => {}
        }
    }
}

/// The build pane's data, rendered straight from the build plugin's
/// `progress`/`line`/`status` events. Port of the Python `BuildModel`.
#[derive(Debug, Clone, Default)]
pub struct BuildModel {
    pub built: i64,
    pub finished: i64,
    pub fetched: i64,
    pub fetched_done: i64,
    pub errors: i64,
    pub current: String,
    pub lines: VecDeque<String>,
    pub done: bool,
    pub rc: Option<i64>,
}

impl BuildModel {
    /// Feed one already-parsed build event: `progress` copies the counters +
    /// `current`, `line` appends, `status done` records completion + rc.
    pub fn feed(&mut self, event: &Value) {
        match event.get("event").and_then(Value::as_str) {
            Some("progress") => {
                if let Some(v) = event.get("built").and_then(Value::as_i64) {
                    self.built = v;
                }
                if let Some(v) = event.get("finished").and_then(Value::as_i64) {
                    self.finished = v;
                }
                if let Some(v) = event.get("fetched").and_then(Value::as_i64) {
                    self.fetched = v;
                }
                if let Some(v) = event.get("fetched_done").and_then(Value::as_i64) {
                    self.fetched_done = v;
                }
                if let Some(v) = event.get("errors").and_then(Value::as_i64) {
                    self.errors = v;
                }
                if let Some(c) = event.get("current").and_then(Value::as_str) {
                    self.current = c.to_string();
                }
            }
            Some("line") => {
                let line = event.get("line").and_then(Value::as_str).unwrap_or("");
                push_capped(&mut self.lines, line.to_string(), BUILD_MAX_LINES);
            }
            Some("status") if event.get("state").and_then(Value::as_str) == Some("done") => {
                self.done = true;
                self.rc = event.get("rc").and_then(Value::as_i64);
            }
            _ => {}
        }
    }
}

/// Incremental reader over a run's events directory: per-file byte offsets,
/// version-gated records, routed to a [`BuildModel`] / per-host [`HostRun`].
/// Port of the Python `EventTailer`.
pub struct EventTailer {
    pub directory: PathBuf,
    offsets: BTreeMap<PathBuf, u64>,
    pub hosts: BTreeMap<String, HostRun>,
    pub build: BuildModel,
    /// A callback receiving every raw `nixlog` line live (nom food). Attach it
    /// BEFORE polling starts; `None` drops nixlog records.
    pub nixlog_sink: Option<Box<dyn FnMut(String)>>,
}

impl std::fmt::Debug for EventTailer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventTailer")
            .field("directory", &self.directory)
            .field("hosts", &self.hosts)
            .field("build", &self.build)
            .field("nixlog_sink", &self.nixlog_sink.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl EventTailer {
    #[must_use]
    pub fn new(directory: &Path) -> Self {
        EventTailer {
            directory: directory.to_path_buf(),
            offsets: BTreeMap::new(),
            hosts: BTreeMap::new(),
            build: BuildModel::default(),
            nixlog_sink: None,
        }
    }

    /// Get-or-create the [`HostRun`] for a host name.
    pub fn host(&mut self, name: &str) -> &mut HostRun {
        self.hosts
            .entry(name.to_string())
            .or_insert_with(|| HostRun::new(name))
    }

    /// Consume newly appended events across every `*.jsonl` file (sorted, for
    /// deterministic cross-file ordering). Returns how many records were read.
    ///
    /// Each file resumes from its recorded byte offset; a line without a
    /// trailing newline is a partial write — we stop before it and re-read it
    /// on the next poll (its bytes are not counted toward the offset).
    pub fn poll(&mut self) -> usize {
        let mut count = 0usize;
        if !self.directory.is_dir() {
            return 0;
        }
        let mut paths: Vec<PathBuf> = match std::fs::read_dir(&self.directory) {
            Ok(rd) => rd
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
                .collect(),
            Err(_) => return 0,
        };
        paths.sort();
        for path in paths {
            let mut offset = self.offsets.get(&path).copied().unwrap_or(0);
            let file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let mut reader = BufReader::new(file);
            if reader.seek(SeekFrom::Start(offset)).is_err() {
                continue;
            }
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        if !line.ends_with('\n') {
                            break; // partial write; re-read next poll
                        }
                        // Byte length of the UTF-8 line (parity with Python
                        // `len(line.encode("utf-8"))`).
                        offset += line.len() as u64;
                        count += 1;
                        self.route(&line);
                    }
                    Err(_) => break,
                }
            }
            self.offsets.insert(path, offset);
        }
        count
    }

    /// Parse one raw line and route it: drop unparseable / unsupported-version
    /// records; `nixlog` → the sink (and nowhere else); `plugin == "build"` →
    /// the build model; else a `host`-tagged record → that host's run.
    fn route(&mut self, line: &str) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return;
        };
        if !version_supported(event.get("v").and_then(Value::as_i64)) {
            return;
        }
        if event.get("event").and_then(Value::as_str) == Some("nixlog") {
            if let Some(sink) = self.nixlog_sink.as_mut() {
                let l = event.get("line").and_then(Value::as_str).unwrap_or("");
                sink(l.to_string());
            }
            return;
        }
        if event.get("plugin").and_then(Value::as_str) == Some("build") {
            self.build.feed(&event);
            return;
        }
        if let Some(host) = event.get("host").and_then(Value::as_str)
            && !host.is_empty()
        {
            self.host(host).feed(&event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mandala-runner-test-{}-{:?}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Append v1 events (defaulting `v`/`ts`) to a `.jsonl` file — the port of
    /// the Python test `_write` helper.
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
            if let Some(m) = e.as_object() {
                for (k, v) in m {
                    obj.insert(k.clone(), v.clone());
                }
            }
            writeln!(fh, "{}", Value::Object(obj)).unwrap();
        }
    }

    fn milestones(host: &str, names: &[&str]) -> Vec<Value> {
        names
            .iter()
            .map(|n| {
                serde_json::json!({
                    "host": host, "plugin": "deploy",
                    "event": "milestone", "milestone": n,
                })
            })
            .collect()
    }

    /// Port of `test_multi_host_demux_with_rollback`: a fan-out where one host
    /// rolls back must flag that host (stickily, despite a later rc=1) without
    /// disturbing the others; and an incremental poll advances only the touched
    /// host.
    #[test]
    fn multi_host_demux_with_rollback() {
        let dir = tmp();
        // Build play events land in the first host's file (run_once).
        write_events(
            &dir.join("alpha.jsonl"),
            &[
                serde_json::json!({"host":"alpha","plugin":"build","event":"status","state":"start","cmd":[]}),
                serde_json::json!({"host":"alpha","plugin":"build","event":"progress",
                    "built":4,"finished":4,"fetched":9,"fetched_done":9,"errors":0,"current":"system-path"}),
                serde_json::json!({"host":"alpha","plugin":"build","event":"status","state":"done","rc":0}),
            ],
        );
        write_events(
            &dir.join("alpha.jsonl"),
            &milestones(
                "alpha",
                &["eval", "build", "copy", "activate", "wait", "confirm"],
            ),
        );
        write_events(
            &dir.join("alpha.jsonl"),
            &[
                serde_json::json!({"host":"alpha","plugin":"deploy","event":"status","state":"done","rc":0}),
            ],
        );
        write_events(
            &dir.join("beta.jsonl"),
            &milestones("beta", &["eval", "copy", "activate", "rollback"]),
        );
        write_events(
            &dir.join("beta.jsonl"),
            &[
                serde_json::json!({"host":"beta","plugin":"deploy","event":"line","line":"magic rollback fired","stream":"deploy"}),
                serde_json::json!({"host":"beta","plugin":"deploy","event":"status","state":"done","rc":1}),
            ],
        );
        write_events(
            &dir.join("gamma.jsonl"),
            &milestones("gamma", &["eval", "copy"]),
        );

        let mut tailer = EventTailer::new(&dir);
        tailer.poll();

        assert!(tailer.build.done && tailer.build.rc == Some(0));
        assert_eq!(tailer.build.finished, 4);
        assert_eq!(tailer.build.fetched, 9);

        assert_eq!(tailer.hosts["alpha"].state, HostState::Confirmed);
        // The rolled-back host is flagged — and stays flagged despite rc=1.
        assert_eq!(tailer.hosts["beta"].state, HostState::RolledBack);
        assert!(
            tailer.hosts["beta"]
                .lines
                .iter()
                .any(|l| l == "magic rollback fired")
        );
        // The others are untouched by beta's failure.
        assert_eq!(tailer.hosts["gamma"].state, HostState::Copying);

        // Incremental: appended events advance only the touched host.
        write_events(
            &dir.join("gamma.jsonl"),
            &milestones("gamma", &["activate", "confirm"]),
        );
        tailer.poll();
        assert_eq!(tailer.hosts["gamma"].state, HostState::Confirmed);
        assert_eq!(tailer.hosts["beta"].state, HostState::RolledBack);
    }

    /// Port of `test_nixlog_routes_to_sink_and_nowhere_else`: a v2 `nixlog`
    /// record reaches the sink and never pollutes the line/host views.
    #[test]
    fn nixlog_routes_to_sink_and_nowhere_else() {
        let dir = tmp();
        // A v2 record; write it verbatim (not through the v1 default helper).
        {
            use std::io::Write;
            let mut fh = std::fs::File::create(dir.join("alpha.jsonl")).unwrap();
            let ev = serde_json::json!({
                "v": 2, "host": "alpha", "plugin": "build", "event": "nixlog",
                "line": r#"@nix {"action":"start","type":105}"#,
            });
            writeln!(fh, "{ev}").unwrap();
        }
        let mut tailer = EventTailer::new(&dir);
        let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        tailer.nixlog_sink = Some(Box::new(move |s| sink.borrow_mut().push(s)));
        tailer.poll();
        assert_eq!(
            *seen.borrow(),
            vec![r#"@nix {"action":"start","type":105}"#.to_string()]
        );
        assert!(tailer.build.lines.is_empty()); // nixlog never pollutes the line views
        assert!(!tailer.hosts.contains_key("alpha"));
    }

    /// Port of `test_failed_without_rollback_and_version_gate`: a bare rc=2
    /// flags FAILED, and a future-versioned record is ignored (not misread as a
    /// confirm that would unflag the failure).
    #[test]
    fn failed_without_rollback_and_version_gate() {
        let dir = tmp();
        write_events(
            &dir.join("delta.jsonl"),
            &milestones("delta", &["eval", "copy"]),
        );
        write_events(
            &dir.join("delta.jsonl"),
            &[
                serde_json::json!({"host":"delta","plugin":"deploy","event":"status","state":"done","rc":2}),
            ],
        );
        // Future-versioned records must be ignored, not misread.
        {
            use std::io::Write;
            let mut fh = std::fs::OpenOptions::new()
                .append(true)
                .open(dir.join("delta.jsonl"))
                .unwrap();
            let ev = serde_json::json!({"v":99,"host":"delta","plugin":"deploy","event":"milestone","milestone":"confirm"});
            writeln!(fh, "{ev}").unwrap();
        }
        let mut tailer = EventTailer::new(&dir);
        tailer.poll();
        assert_eq!(tailer.hosts["delta"].state, HostState::Failed);
    }

    /// A line written without a trailing newline (a write in flight) is NOT
    /// consumed; the byte offset does not advance past it; the next poll reads
    /// it once it is completed — and resumes exactly from the recorded offset.
    #[test]
    fn partial_trailing_line_reread_and_offset_resume() {
        use std::io::Write;
        let dir = tmp();
        let path = dir.join("alpha.jsonl");
        // One complete event, then a partial (no newline).
        write_events(&dir.join("alpha.jsonl"), &milestones("alpha", &["eval"]));
        let partial = serde_json::json!({"v":1,"host":"alpha","plugin":"deploy","event":"milestone","milestone":"build"});
        {
            let mut fh = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            write!(fh, "{partial}").unwrap(); // no trailing newline
        }
        let mut tailer = EventTailer::new(&dir);
        let n = tailer.poll();
        assert_eq!(n, 1); // only the complete event
        assert_eq!(tailer.hosts["alpha"].state, HostState::Evaluating);

        // Complete the partial line; the next poll consumes exactly it.
        {
            let mut fh = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(fh).unwrap();
        }
        let n2 = tailer.poll();
        assert_eq!(n2, 1);
        assert_eq!(tailer.hosts["alpha"].state, HostState::Building);
    }
}
