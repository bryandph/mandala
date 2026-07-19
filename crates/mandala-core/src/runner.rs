//! Deploy-runner: drive the fan-out playbook (and ad-hoc background commands)
//! as subprocesses, and demux the per-host JSONL event streams a run writes
//! into its registry dir, tailing them incrementally into host state machines
//! and a build-progress model.
//!
//! A parity port of the retired Python `mandala_fleet.runner`. The READ half
//! (`HostState`/`HostRun`, `BuildModel`, `EventTailer`) is what any frontend —
//! a second TUI, the CLI, the fleet MCP server — uses to render an in-flight or
//! finished run from the shared event files, without owning the subprocess that
//! produced them. The WRITE half ([`DeployRun`], [`CommandRun`]) owns the
//! subprocess: it launches the playbook / command on **tokio** (the design's
//! single-runtime mandate — the MCP server and phase-2 TUI drive these async),
//! registers the run in the discoverable registry so other frontends can
//! attach, and drains/reaps it in background tasks.
//!
//! The event JSONL protocol (versions 1 and 2, gated by the `v` field) is the
//! whole cross-implementation contract: the Python porcelain writes it, this
//! reads it byte-compatibly, and vice versa. Unknown fields are tolerated (real
//! files carry more), unsupported versions are skipped rather than misread, and
//! a partial trailing line (a write in flight) is re-read on the next poll.

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncBufReadExt;

use crate::registry::{self, Meta};

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
    /// BEFORE polling starts; `None` drops nixlog records. `Send` so an
    /// [`ObservedRun`](crate::registry::ObservedRun) / [`DeployRun`] can be
    /// held across await points (the MCP server's blocking waits).
    pub nixlog_sink: Option<Box<dyn FnMut(String) + Send>>,
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

// ==== write-side: subprocess-owning runners ==============================
//
// `DeployRun` (fan-out playbook) launches a dedicated supervisor process;
// `CommandRun` (ad-hoc background command, e.g. reboot) owns a tokio
// subprocess directly. Both register the run in the discoverable registry so
// other frontends can attach. Parity port of `runner.py` lines 35-62
// (helpers) and 210-449.

/// The operator repo's ansible root when present, else the cwd — the one
/// working-directory convention every frontend shares. Parity of the Python
/// `ansible_dir()`.
#[must_use]
pub fn ansible_dir() -> PathBuf {
    if Path::new("ansible/ansible.cfg").is_file() {
        PathBuf::from("ansible")
    } else {
        PathBuf::from(".")
    }
}

/// The reboot launch line, shared by the TUI action and the MCP reboot tool.
///
/// Prefers the operator's `ans-reboot` wrapper: it carries controller-side env
/// raw `ansible-playbook` lacks — the delegated k8s drain pins a python WITH
/// the kubernetes lib. Falls back to `playbooks/reboot.yaml`; `None` when
/// neither exists. Parity of the Python `reboot_argv`.
#[must_use]
pub fn reboot_argv(target: &str, serial: &str, drain: bool) -> Option<Vec<String>> {
    let mut base = if which("ans-reboot") {
        vec![
            "ans-reboot".to_string(),
            "-l".to_string(),
            target.to_string(),
        ]
    } else if ansible_dir().join("playbooks/reboot.yaml").is_file() {
        vec![
            "ansible-playbook".to_string(),
            "playbooks/reboot.yaml".to_string(),
            "-l".to_string(),
            target.to_string(),
        ]
    } else {
        return None;
    };
    base.push("-e".to_string());
    base.push(format!("reboot_serial={serial}"));
    base.push("-e".to_string());
    base.push(format!("drain={}", if drain { "true" } else { "false" }));
    Some(base)
}

/// Is `name` an executable on `PATH`? (Parity of `shutil.which`, used only to
/// prefer the `ans-reboot` wrapper over the raw playbook.)
fn which(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        std::fs::metadata(dir.join(name))
            .map(|m| m.is_file() && (m.permissions().mode() & 0o111 != 0))
            .unwrap_or(false)
    })
}

/// Unix epoch seconds as a float — parity with Python `time.time()`, the
/// `started_at`/`finished_at` meta value.
fn now_epoch_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Map a subprocess exit status to a return code the way Python's
/// `Popen.poll()` does: the exit code, or `-signum` when signalled.
fn exit_code(status: std::process::ExitStatus) -> i64 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .map_or_else(|| i64::from(-status.signal().unwrap_or(0)), i64::from)
}

/// Merge `extra` into `meta`, later keys winning — parity of Python's
/// `{..., **extra_meta}` spread (operator-supplied fields override the base).
fn merge_meta(meta: &mut Meta, extra: &Meta) {
    for (k, v) in extra {
        meta.insert(k.clone(), v.clone());
    }
}

const DEPLOY_SUPERVISOR_REQUEST: &str = "supervisor.json";

#[derive(Debug, Deserialize, Serialize)]
struct DeploySupervisorRequest {
    argv: Vec<String>,
    cwd: PathBuf,
    events_dir: PathBuf,
}

fn supervisor_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MANDALA_DEPLOY_SUPERVISOR_BIN") {
        return Some(PathBuf::from(path));
    }
    let executable = std::env::current_exe().ok()?;
    let directory = executable.parent()?;
    let sibling = directory.join("mandala-run-supervisor");
    if sibling.is_file() {
        return Some(sibling);
    }
    if directory.file_name().is_some_and(|name| name == "deps") {
        let sibling = directory.parent()?.join("mandala-run-supervisor");
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    None
}

/// Run one deploy request from a dedicated process. The supervisor owns the
/// actual playbook child, durable log descriptors, reaping, and terminal
/// metadata, so none of those depend on the frontend's Tokio runtime.
///
/// # Errors
/// Invalid registry paths, request decoding, log setup, spawn, wait, or meta
/// settlement failures.
pub fn run_deploy_supervisor(run_dir: &Path) -> std::io::Result<i64> {
    let run_dir = run_dir.canonicalize()?;
    let run_id = run_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::other("run directory has no UTF-8 basename"))?;
    if !registry::is_valid_run_id(run_id) {
        return Err(std::io::Error::other("invalid run directory identifier"));
    }
    let registry_root = registry::runs_dir().canonicalize()?;
    if run_dir.parent() != Some(registry_root.as_path()) {
        return Err(std::io::Error::other(
            "run directory is outside the registry",
        ));
    }

    let request_path = run_dir.join(DEPLOY_SUPERVISOR_REQUEST);
    let request: DeploySupervisorRequest =
        serde_json::from_slice(&std::fs::read(&request_path)?).map_err(std::io::Error::other)?;
    let _ = std::fs::remove_file(&request_path);
    if request.argv.is_empty() || request.events_dir.canonicalize()? != run_dir {
        return Err(std::io::Error::other("invalid deploy supervisor request"));
    }

    let log_path = run_dir.join(COMMAND_LOG);
    let out = std::fs::OpenOptions::new().append(true).open(&log_path)?;
    let err = out.try_clone()?;
    let mut command = std::process::Command::new(&request.argv[0]);
    command
        .args(&request.argv[1..])
        .current_dir(&request.cwd)
        .env("MANDALA_FLEET_EVENTS", &run_dir)
        .env("PYTHONUNBUFFERED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    if std::env::var_os("ANSIBLE_FORCE_COLOR").is_none() {
        command.env("ANSIBLE_FORCE_COLOR", "0");
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            if let Ok(mut log) = std::fs::OpenOptions::new().append(true).open(&log_path) {
                let _ = writeln!(log, "failed to launch {}: {error}", request.argv[0]);
            }
            let mut fields = Meta::new();
            fields.insert(
                "supervisor_pid".into(),
                Value::from(i64::from(std::process::id())),
            );
            fields.insert("pid".into(), Value::Null);
            fields.insert("rc".into(), Value::from(127));
            fields.insert("error".into(), Value::from(error.to_string()));
            fields.insert("finished_at".into(), Value::from(now_epoch_f64()));
            registry::update_meta(&run_dir, fields)?;
            return Ok(127);
        }
    };

    let mut running = Meta::new();
    running.insert(
        "supervisor_pid".into(),
        Value::from(i64::from(std::process::id())),
    );
    running.insert("pid".into(), Value::from(i64::from(child.id())));
    registry::update_meta(&run_dir, running)?;

    let status = child.wait()?;
    let code = exit_code(status);
    let mut terminal = Meta::new();
    terminal.insert("rc".into(), Value::from(code));
    terminal.insert("finished_at".into(), Value::from(now_epoch_f64()));
    registry::update_meta(&run_dir, terminal)?;
    Ok(code)
}

/// Lines kept in a [`DeployRun`]'s stdout mirror (Python `deque(maxlen=4000)`).
const DEPLOY_OUTPUT_MAX: usize = 4000;

/// Spawn a background task draining `reader`'s lines into the shared bounded
/// buffer. Used for a [`DeployRun`]'s piped stdout and stderr.
fn spawn_line_drain<R>(reader: R, buf: Arc<Mutex<VecDeque<String>>>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(mut o) = buf.lock() {
                push_capped(&mut o, line, DEPLOY_OUTPUT_MAX);
            }
        }
    });
}

/// One fan-out deploy: an owned supervisor process plus the [`EventTailer`]
/// over the run dir the supervised playbook streams into — or, in *attached*
/// mode, a read-only observer of a run another process launched (a
/// Claude-triggered deploy), owning no subprocess. Parity port of the Python
/// `DeployRun`.
///
/// The playbook stays the engine: the `--limit` guard, throttle, and deploy-rs
/// magic rollback are never bypassed; this only launches it (with
/// `MANDALA_FLEET_EVENTS` pointed at a run dir) and tails the event files the
/// `mandala.fleet` plugins append.
pub struct DeployRun {
    pub limit: String,
    pub dry_activate: bool,
    pub throttle: i64,
    pub ansible_dir: Option<PathBuf>,
    pub playbook: Option<String>,
    pub events_dir: Option<PathBuf>,
    pub run_id: Option<String>,
    pub tailer: Option<EventTailer>,
    /// Override the launched argv verbatim (tests, and phase-4 native
    /// launchers); `None` builds the `ansible-playbook …` line exactly as the
    /// Python porcelain. Never enters `meta.json` (deploy meta records no argv).
    pub program: Option<Vec<String>>,
    /// The bounded stdout+stderr mirror the reader tasks fill; shared with them,
    /// snapshot via [`DeployRun::output`].
    output: Arc<Mutex<VecDeque<String>>>,
    /// Byte offset already copied from the supervisor-owned durable log.
    output_offset: Mutex<u64>,
    /// Owned-mode supervisor; `None` in attached mode / before `start`.
    child: Option<tokio::process::Child>,
    /// Cached exit status once reaped (tokio's `try_wait` yields it once).
    exited: Option<std::process::ExitStatus>,
    /// Attached mode: liveness/returncode derive from the registry pid + sticky
    /// host states, never a locally-owned subprocess.
    attached: bool,
    meta_pid: Option<i64>,
}

impl DeployRun {
    /// A deploy of `limit` with default throttle (4), owned-subprocess mode.
    #[must_use]
    pub fn new(limit: impl Into<String>) -> Self {
        DeployRun {
            limit: limit.into(),
            dry_activate: false,
            throttle: 4,
            ansible_dir: None,
            playbook: None,
            events_dir: None,
            run_id: None,
            tailer: None,
            program: None,
            output: Arc::new(Mutex::new(VecDeque::new())),
            output_offset: Mutex::new(0),
            child: None,
            exited: None,
            attached: false,
            meta_pid: None,
        }
    }

    /// Read-only attach to an already-launched registry run: adopt its tailer +
    /// meta pid, own no subprocess, so a run started by another frontend renders
    /// identically. `None` if the run is gone. Parity of `DeployRun.attach`.
    #[must_use]
    pub fn attach(run_id: &str) -> Option<Self> {
        let obs = registry::open_run(run_id)?;
        let meta = &obs.info.meta;
        let mut run = DeployRun::new(
            meta.get("limit")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        run.dry_activate = meta
            .get("dry_activate")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        run.events_dir = Some(obs.info.path.clone());
        run.run_id = Some(run_id.to_string());
        run.meta_pid = meta.get("pid").and_then(Value::as_i64);
        run.tailer = Some(obs.tailer);
        run.attached = true;
        Some(run)
    }

    /// Resolve `ansible_dir` / `playbook` / `events_dir` defaults (the last by
    /// allocating a registry run dir). Parity of `resolve_paths`.
    ///
    /// # Errors
    /// Any filesystem error allocating the registry run dir.
    pub fn resolve_paths(&mut self) -> std::io::Result<()> {
        if self.ansible_dir.is_none() {
            self.ansible_dir = Some(ansible_dir());
        }
        if self.playbook.is_none() {
            let dir = self.ansible_dir.as_ref().expect("ansible_dir just set");
            self.playbook = Some(if dir.join("playbooks/deploy.yaml").is_file() {
                "playbooks/deploy.yaml".to_string()
            } else {
                "mandala.fleet.deploy".to_string()
            });
        }
        if self.events_dir.is_none() {
            let (run_id, dir) = registry::new_run_dir()?;
            self.run_id = Some(run_id);
            self.events_dir = Some(dir);
        }
        Ok(())
    }

    /// The launched argv: the `program` override, else the `ansible-playbook`
    /// line (parity of the Python argv construction).
    fn argv(&self) -> Vec<String> {
        if let Some(p) = &self.program {
            return p.clone();
        }
        let mut argv = vec![
            "ansible-playbook".to_string(),
            self.playbook.clone().unwrap_or_default(),
            "-l".to_string(),
            self.limit.clone(),
            "-e".to_string(),
            format!("deploy_throttle={}", self.throttle),
        ];
        if self.dry_activate {
            argv.push("-e".to_string());
            argv.push("deploy_dry_activate=true".to_string());
        }
        argv
    }

    /// Launch the deploy: resolve paths, spawn the subprocess (stdin **null** —
    /// never inherit an ssh/vault/become prompt), register the run in the
    /// registry keyed on the live pid, and start the background readers draining
    /// stdout+stderr into the mirror. Parity of `DeployRun.start`.
    ///
    /// # Errors
    /// A path-resolution or spawn failure. A *registry-write* failure is
    /// swallowed — it must never sink an already-launched run.
    pub async fn start(&mut self) -> std::io::Result<()> {
        // Unit tests use a thread-local registry root that cannot cross a
        // process boundary; keep their stub runner inline. Production and
        // integration tests use the packaged sibling supervisor.
        if cfg!(test) {
            return self.start_inline().await;
        }
        let Some(supervisor) = supervisor_binary() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "mandala-run-supervisor is not installed beside the frontend",
            ));
        };
        self.start_supervised(supervisor).await
    }

    async fn start_supervised(&mut self, supervisor: PathBuf) -> std::io::Result<()> {
        self.resolve_paths()?;
        let events_dir = self.events_dir.clone().expect("events_dir resolved");
        self.tailer = Some(EventTailer::new(&events_dir));
        let argv = self.argv();
        let ansible_dir = self
            .ansible_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));
        let log_path = events_dir.join(COMMAND_LOG);
        {
            let mut log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)?;
            writeln!(
                log,
                "$ {}  (cwd={}, events={})",
                argv.join(" "),
                ansible_dir.display(),
                events_dir.display()
            )?;
            log.flush()?;
        }
        let request = DeploySupervisorRequest {
            argv,
            cwd: ansible_dir,
            events_dir: events_dir.clone(),
        };
        let request_tmp = events_dir.join(format!("{DEPLOY_SUPERVISOR_REQUEST}.tmp"));
        std::fs::write(
            &request_tmp,
            serde_json::to_vec(&request).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(&request_tmp, events_dir.join(DEPLOY_SUPERVISOR_REQUEST))?;

        let mut meta = Meta::new();
        meta.insert(
            "run_id".into(),
            Value::from(self.run_id.clone().unwrap_or_default()),
        );
        meta.insert("limit".into(), Value::from(self.limit.clone()));
        meta.insert("dry_activate".into(), Value::from(self.dry_activate));
        meta.insert("throttle".into(), Value::from(self.throttle));
        meta.insert(
            "playbook".into(),
            Value::from(self.playbook.clone().unwrap_or_default()),
        );
        meta.insert("pid".into(), Value::Null);
        // The launcher owns this short initialization window. Recording it as
        // the lifecycle owner prevents another frontend from seeing the run as
        // finished before the dedicated supervisor is spawned and takes over.
        meta.insert(
            "supervisor_pid".into(),
            Value::from(i64::from(std::process::id())),
        );
        meta.insert("started_at".into(), Value::from(now_epoch_f64()));
        registry::write_meta(&events_dir, &meta)?;

        let supervisor_out = std::fs::OpenOptions::new().append(true).open(&log_path)?;
        let supervisor_err = supervisor_out.try_clone()?;
        let child = match tokio::process::Command::new(supervisor)
            .arg(&events_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(supervisor_out))
            .stderr(Stdio::from(supervisor_err))
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                let _ = std::fs::remove_file(events_dir.join(DEPLOY_SUPERVISOR_REQUEST));
                if let Ok(mut log) = std::fs::OpenOptions::new().append(true).open(&log_path) {
                    let _ = writeln!(log, "failed to launch deploy supervisor: {error}");
                }
                let mut terminal = Meta::new();
                terminal.insert("rc".into(), Value::from(127));
                terminal.insert("error".into(), Value::from(error.to_string()));
                terminal.insert("finished_at".into(), Value::from(now_epoch_f64()));
                registry::update_meta(&events_dir, terminal)?;
                return Err(error);
            }
        };
        let supervisor_pid = i64::from(child.id().expect("newly spawned supervisor has a pid"));
        let mut running = Meta::new();
        running.insert("supervisor_pid".into(), Value::from(supervisor_pid));
        registry::update_meta(&events_dir, running)?;
        self.child = Some(child);
        Ok(())
    }

    async fn start_inline(&mut self) -> std::io::Result<()> {
        self.resolve_paths()?;
        let events_dir = self.events_dir.clone().expect("events_dir resolved");
        self.tailer = Some(EventTailer::new(&events_dir));

        let argv = self.argv();
        let ansible_dir = self
            .ansible_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));
        self.push_output(format!(
            "$ {}  (cwd={}, events={})",
            argv.join(" "),
            ansible_dir.display(),
            events_dir.display(),
        ));

        let mut cmd = tokio::process::Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(&ansible_dir)
            .env("MANDALA_FLEET_EVENTS", &events_dir)
            // ansible block-buffers stdout when piped — without this, output
            // arrives in late multi-KB chunks and the view looks dead.
            .env("PYTHONUNBUFFERED", "1")
            // NEVER inherit stdin: an interactive prompt (ssh, vault, become)
            // would wedge the run silently.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if std::env::var_os("ANSIBLE_FORCE_COLOR").is_none() {
            cmd.env("ANSIBLE_FORCE_COLOR", "0"); // setdefault
        }
        let mut child = cmd.spawn()?;

        // Register so other frontends discover + tail the same events, keyed on
        // the live pid. A write failure must never sink the run itself.
        let pid = child.id();
        let mut meta = Meta::new();
        meta.insert(
            "run_id".into(),
            Value::from(self.run_id.clone().unwrap_or_default()),
        );
        meta.insert("limit".into(), Value::from(self.limit.clone()));
        meta.insert("dry_activate".into(), Value::from(self.dry_activate));
        meta.insert("throttle".into(), Value::from(self.throttle));
        meta.insert(
            "playbook".into(),
            Value::from(self.playbook.clone().unwrap_or_default()),
        );
        meta.insert(
            "pid".into(),
            pid.map_or(Value::Null, |p| Value::from(i64::from(p))),
        );
        meta.insert("started_at".into(), Value::from(now_epoch_f64()));
        let _ = registry::write_meta(&events_dir, &meta);

        // Reader tasks: drain stdout AND stderr into the shared bounded buffer.
        // (Python merges stderr into stdout at the OS level; two tasks over one
        // buffer is the tokio-idiomatic equivalent — every line still lands.)
        if let Some(out) = child.stdout.take() {
            spawn_line_drain(out, Arc::clone(&self.output));
        }
        if let Some(err) = child.stderr.take() {
            spawn_line_drain(err, Arc::clone(&self.output));
        }
        self.child = Some(child);
        Ok(())
    }

    fn push_output(&self, line: String) {
        if let Ok(mut o) = self.output.lock() {
            push_capped(&mut o, line, DEPLOY_OUTPUT_MAX);
        }
    }

    fn sync_output_log(&self) {
        let Some(path) = self.events_dir.as_ref().map(|dir| dir.join(COMMAND_LOG)) else {
            return;
        };
        let Ok(mut file) = std::fs::File::open(path) else {
            return;
        };
        let Ok(mut offset) = self.output_offset.lock() else {
            return;
        };
        if file.seek(SeekFrom::Start(*offset)).is_err() {
            return;
        }
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        loop {
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    *offset += read as u64;
                    let clean = line.trim_end_matches(['\r', '\n']).to_string();
                    if let Ok(mut output) = self.output.lock() {
                        push_capped(&mut output, clean, DEPLOY_OUTPUT_MAX);
                    }
                    line.clear();
                }
            }
        }
    }

    /// A snapshot of the bounded stdout+stderr mirror.
    #[must_use]
    pub fn output(&self) -> Vec<String> {
        self.sync_output_log();
        self.output
            .lock()
            .map(|o| o.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Consume newly appended events into the tailer; returns how many were
    /// read (Python's `poll` returns nothing — the count is a convenience).
    pub fn poll(&mut self) -> usize {
        self.sync_output_log();
        self.tailer.as_mut().map_or(0, EventTailer::poll)
    }

    /// Cache + return the owned child's exit status once it has exited (tokio's
    /// `try_wait` yields the status once, so it is memoized here).
    fn poll_exit(&mut self) -> Option<std::process::ExitStatus> {
        if self.exited.is_none()
            && let Some(child) = self.child.as_mut()
            && let Ok(Some(status)) = child.try_wait()
        {
            self.exited = Some(status);
        }
        self.exited
    }

    /// Whether the run has finished. Parity of the `finished` property: attached
    /// mode = the registry pid is gone; owned mode = the child has exited.
    pub fn finished(&mut self) -> bool {
        if self.attached {
            let meta = self
                .events_dir
                .as_deref()
                .map(registry::read_meta)
                .unwrap_or_default();
            if meta.get("rc").and_then(Value::as_i64).is_some() {
                return true;
            }
            let supervisor = meta.get("supervisor_pid").and_then(Value::as_i64);
            if supervisor.is_some() && !registry::pid_alive(supervisor) {
                return true;
            }
            self.meta_pid = meta.get("pid").and_then(Value::as_i64).or(self.meta_pid);
            return !registry::pid_alive(self.meta_pid);
        }
        self.poll_exit().is_some()
    }

    /// The run's exit code, or `None` while still running. Parity of the
    /// `returncode` property: attached mode derives it from the sticky terminal
    /// host states (any failed/rolled-back ⇒ 1), owned mode from the child.
    pub fn returncode(&mut self) -> Option<i64> {
        if self.attached {
            if !self.finished() {
                return None;
            }
            if let Some(meta) = self.events_dir.as_deref().map(registry::read_meta)
                && let Some(rc) = meta.get("rc").and_then(Value::as_i64)
            {
                return Some(rc);
            }
            if let Some(meta) = self.events_dir.as_deref().map(registry::read_meta)
                && let Some(supervisor) = meta.get("supervisor_pid").and_then(Value::as_i64)
                && !registry::pid_alive(Some(supervisor))
            {
                return Some(1);
            }
            let bad = self.tailer.as_ref().is_some_and(|t| {
                t.hosts
                    .values()
                    .any(|h| matches!(h.state, HostState::Failed | HostState::RolledBack))
            });
            return Some(i64::from(bad));
        }
        self.poll_exit().map(exit_code)
    }

    /// Signal the owned subprocess to terminate (SIGTERM, parity of
    /// `subprocess.Popen.terminate`). A no-op in attached mode (an observer
    /// never owns the subprocess) or once it has exited.
    pub fn terminate(&mut self) {
        if self.attached || self.poll_exit().is_some() {
            return;
        }
        let deployed_pid = self
            .events_dir
            .as_deref()
            .map(registry::read_meta)
            .and_then(|meta| meta.get("pid").and_then(Value::as_i64));
        if let Some(pid) = deployed_pid.and_then(|pid| i32::try_from(pid).ok()) {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGTERM,
            );
        } else if let Some(child) = self.child.as_ref()
            && let Some(pid) = child.id()
        {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
    }
}

/// Command-run output file: sits beside the event JSONLs in the run dir
/// (`EventTailer` globs `*.jsonl`, so it never routes this). Parity of the
/// Python `COMMAND_LOG`.
pub const COMMAND_LOG: &str = "output.log";

/// A registered background command (the reboot playbook, a build): the argv
/// runs on tokio with stdout+stderr teed to `output.log` under a registry run
/// dir, `meta.json` carries kind + pid so any frontend can discover and tail
/// it, and a reaper task records the exit code into meta when the subprocess
/// exits. The launching client (an MCP call, a TUI screen) can therefore vanish
/// — timeout, quit — without orphaning the run unobservably or losing its
/// output. Parity port of the Python `CommandRun`.
pub struct CommandRun {
    pub argv: Vec<String>,
    pub kind: String,
    pub cwd: Option<PathBuf>,
    pub extra_meta: Meta,
    pub run_id: Option<String>,
    pub run_dir: Option<PathBuf>,
    launched: bool,
}

impl CommandRun {
    /// A command run of `argv` tagged `kind` (e.g. `"reboot"`, `"build"`).
    #[must_use]
    pub fn new(argv: Vec<String>, kind: impl Into<String>) -> Self {
        CommandRun {
            argv,
            kind: kind.into(),
            cwd: None,
            extra_meta: Meta::new(),
            run_id: None,
            run_dir: None,
            launched: false,
        }
    }

    /// The teed log path (`None` before `start`).
    #[must_use]
    pub fn log_path(&self) -> Option<PathBuf> {
        self.run_dir.as_ref().map(|d| d.join(COMMAND_LOG))
    }

    /// Whether the subprocess spawned (parity of the `launched` property).
    #[must_use]
    pub fn launched(&self) -> bool {
        self.launched
    }

    /// Allocate a registry run dir, tee stdout+stderr to `output.log`, spawn the
    /// command (stdin **null**), record kind+pid in meta, and start the reaper
    /// task that records the exit code into meta when the subprocess exits. A
    /// spawn failure is still registered (`rc:127`, `error`). Parity of
    /// `CommandRun.start`.
    ///
    /// # Errors
    /// Only a failure to allocate the registry run dir / open the log file; a
    /// *spawn* failure is recorded in meta and returns `Ok`.
    pub async fn start(&mut self) -> std::io::Result<()> {
        let (run_id, run_dir) = registry::new_run_dir()?;
        self.run_id = Some(run_id.clone());
        self.run_dir = Some(run_dir.clone());
        let log_path = run_dir.join(COMMAND_LOG);

        // Header first (post-mortem breadcrumb), flushed before the child
        // inherits its own append fds into the same file.
        let cwd_display = self
            .cwd
            .as_ref()
            .map_or_else(|| ".".to_string(), |p| p.display().to_string());
        {
            let mut header = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)?;
            writeln!(header, "$ {}  (cwd={cwd_display})", self.argv.join(" "))?;
            header.flush()?;
        }

        let mut cmd = tokio::process::Command::new(&self.argv[0]);
        cmd.args(&self.argv[1..]).env("PYTHONUNBUFFERED", "1");
        if let Some(cwd) = &self.cwd {
            cmd.current_dir(cwd);
        }
        if std::env::var_os("ANSIBLE_FORCE_COLOR").is_none() {
            cmd.env("ANSIBLE_FORCE_COLOR", "0"); // setdefault
        }
        // Both streams tee to the same append-mode log (parity of
        // `stderr=subprocess.STDOUT` → same file).
        let out = std::fs::OpenOptions::new().append(true).open(&log_path)?;
        let err = out.try_clone()?;
        cmd.stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err));

        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                if let Ok(mut log) = std::fs::OpenOptions::new().append(true).open(&log_path) {
                    let _ = writeln!(log, "failed to launch {}: {e}", self.argv[0]);
                }
                let mut meta = Meta::new();
                meta.insert("run_id".into(), Value::from(run_id));
                meta.insert("kind".into(), Value::from(self.kind.clone()));
                meta.insert("pid".into(), Value::Null);
                meta.insert("rc".into(), Value::from(127));
                meta.insert("error".into(), Value::from(e.to_string()));
                meta.insert("started_at".into(), Value::from(now_epoch_f64()));
                merge_meta(&mut meta, &self.extra_meta);
                let _ = registry::write_meta(&run_dir, &meta);
                self.launched = false;
                return Ok(());
            }
        };

        let pid = child.id();
        let mut meta = Meta::new();
        meta.insert("run_id".into(), Value::from(run_id));
        meta.insert("kind".into(), Value::from(self.kind.clone()));
        meta.insert(
            "pid".into(),
            pid.map_or(Value::Null, |p| Value::from(i64::from(p))),
        );
        meta.insert("argv".into(), Value::from(self.argv.clone()));
        meta.insert("started_at".into(), Value::from(now_epoch_f64()));
        merge_meta(&mut meta, &self.extra_meta);
        let _ = registry::write_meta(&run_dir, &meta);
        self.launched = true;

        // Reap in the background: liveness flips from pid-alive to the recorded
        // rc, so an observer's judgement survives the launching client vanishing
        // (only the launcher PROCESS dying loses it). Tolerate the run dir being
        // pruned underneath us.
        tokio::spawn(async move {
            let mut child = child;
            if let Ok(status) = child.wait().await {
                let mut fields = Meta::new();
                fields.insert("rc".into(), Value::from(exit_code(status)));
                fields.insert("finished_at".into(), Value::from(now_epoch_f64()));
                let _ = registry::update_meta(&run_dir, fields);
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
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
        let seen: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        tailer.nixlog_sink = Some(Box::new(move |s| sink.lock().unwrap().push(s)));
        tailer.poll();
        assert_eq!(
            *seen.lock().unwrap(),
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

/// Write-side tests: `DeployRun`/`CommandRun` on tokio. Sandbox-safe — the
/// buildRustPackage checkPhase runs offline with no ansible/nix/network, so
/// every stub is a trivial `sh -c …` (never `ansible-playbook`/`nix`). The run
/// registry is pointed at a private tmp base per test via the thread-local
/// `install_runs_base` seam (no `MANDALA_FLEET_STATE` env → no race with
/// drift's env test), and pid liveness is faked via the `pid_alive` hook where
/// determinism matters (the recycled-pid caveat otherwise makes a dead run's
/// liveness nondeterministic).
#[cfg(test)]
mod write_tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// A unique private registry base for one test.
    fn tmp_base() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mandala-write-test-{}-{:?}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Append v1 milestone events (defaulting `v`/`ts`) to a `.jsonl` file.
    fn write_events(path: &Path, events: &[Value]) {
        use std::io::Write as _;
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

    /// Poll a run dir's meta until the reaper records `rc` (parity of the
    /// Python `_wait_for_rc`), yielding so the reaper task can run.
    async fn wait_for_rc(path: &Path) -> Meta {
        for _ in 0..200 {
            let meta = registry::read_meta(path);
            if meta.contains_key("rc") {
                return meta;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("reaper never recorded rc");
    }

    /// Port of `test_command_run_registers_tees_and_reaps`, extended: a
    /// registered run dir with kind+pid meta, BOTH stdout and stderr teed to
    /// `output.log` (surviving the launcher), the exit code + finished_at reaped
    /// into meta, and `open_run` discovering it with a rc-derived liveness.
    #[tokio::test]
    async fn command_run_registers_tees_and_reaps() {
        let base = tmp_base();
        let _g = registry::test_hooks::install_runs_base(base);
        let mut run = CommandRun::new(
            vec![
                "sh".into(),
                "-c".into(),
                "printf 'rebooting-ish\\n'; printf 'on-stderr\\n' 1>&2; exit 3".into(),
            ],
            "reboot",
        );
        run.extra_meta.insert("limit".into(), Value::from("web"));
        run.start().await.unwrap();
        assert!(run.launched());
        let run_id = run.run_id.clone().unwrap();
        let run_dir = run.run_dir.clone().unwrap();

        let meta = wait_for_rc(&run_dir).await;
        assert_eq!(meta.get("kind").and_then(Value::as_str), Some("reboot"));
        assert_eq!(meta.get("limit").and_then(Value::as_str), Some("web"));
        assert_eq!(meta.get("rc").and_then(Value::as_i64), Some(3));
        assert!(meta.get("finished_at").is_some());
        assert!(meta.get("argv").and_then(Value::as_array).is_some());
        assert!(matches!(meta.get("pid").and_then(Value::as_i64), Some(p) if p > 0));

        // Both streams landed in the teed log; the argv header leads it.
        let log = std::fs::read_to_string(run.log_path().unwrap()).unwrap();
        assert!(log.starts_with("$ "));
        assert!(log.contains("rebooting-ish"));
        assert!(log.contains("on-stderr"));

        // A discoverable run: open_run sees the reboot; pid gone (faked, to dodge
        // the recycled-pid caveat) → rc=3 → Failed.
        let _pg = registry::test_hooks::install(|_| false);
        let mut obs = registry::open_run(&run_id).unwrap();
        assert_eq!(obs.info.kind(), "reboot");
        assert_eq!(obs.liveness(), registry::RunLiveness::Failed);
    }

    /// Port of `test_command_run_failed_launch_is_still_registered`: a spawn
    /// failure is still a registered run (`rc:127`, `error`) with the reason in
    /// the log.
    #[tokio::test]
    async fn command_run_failed_launch_is_still_registered() {
        let base = tmp_base();
        let _g = registry::test_hooks::install_runs_base(base);
        let mut run = CommandRun::new(vec!["/nonexistent/ans-reboot".into()], "reboot");
        run.start().await.unwrap();
        assert!(!run.launched());
        let meta = registry::read_meta(&run.run_dir.clone().unwrap());
        assert_eq!(meta.get("rc").and_then(Value::as_i64), Some(127));
        assert!(meta.get("error").is_some());
        let log = std::fs::read_to_string(run.log_path().unwrap()).unwrap();
        assert!(log.contains("failed to launch"));
    }

    /// A `DeployRun` over a stub program: creates the registry run dir, writes
    /// meta with the live pid + the deploy field set (run_id/limit/dry_activate/
    /// throttle/playbook/pid/started_at, and NO argv), and the reader drains the
    /// stub's stdout into the bounded mirror behind the argv header.
    #[tokio::test]
    async fn deploy_run_registers_meta_and_drains_output() {
        let base = tmp_base();
        let _g = registry::test_hooks::install_runs_base(base);
        let mut run = DeployRun::new("web,cache");
        run.throttle = 7;
        run.dry_activate = true;
        // Stub in place of ansible-playbook: emit two lines, then linger so the
        // recorded pid is verifiably alive.
        run.program = Some(vec![
            "sh".into(),
            "-c".into(),
            "printf 'line-one\\nline-two\\n'; sleep 1".into(),
        ]);
        run.start().await.unwrap();

        let events_dir = run.events_dir.clone().unwrap();
        assert!(events_dir.is_dir());

        let meta = registry::read_meta(&events_dir);
        assert_eq!(meta.get("limit").and_then(Value::as_str), Some("web,cache"));
        assert_eq!(
            meta.get("dry_activate").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(meta.get("throttle").and_then(Value::as_i64), Some(7));
        assert!(meta.get("playbook").and_then(Value::as_str).is_some());
        assert!(meta.get("run_id").and_then(Value::as_str).is_some());
        assert!(meta.get("started_at").is_some());
        // Deploy meta records NO argv (parity — the `program` override never
        // leaks into meta).
        assert!(meta.get("argv").is_none());

        // The recorded pid is the live subprocess pid (still sleeping).
        let pid = meta.get("pid").and_then(Value::as_i64);
        assert!(matches!(pid, Some(p) if p > 0));
        assert!(registry::pid_alive(pid), "recorded pid should be alive");

        // The reader drains stdout lines into the bounded mirror.
        let mut drained = false;
        for _ in 0..200 {
            let out = run.output();
            if out.iter().any(|l| l == "line-one") && out.iter().any(|l| l == "line-two") {
                drained = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(
            drained,
            "reader never drained stub output: {:?}",
            run.output()
        );
        // The argv header leads the mirror.
        assert!(run.output().iter().any(|l| l.starts_with("$ ")));

        run.terminate(); // reap the lingering stub
    }

    /// `DeployRun::attach` owns no subprocess: `finished`/`returncode` derive
    /// from the registry pid then the sticky terminal host states (any
    /// failed/rolled-back ⇒ 1, else 0); `terminate` is a no-op.
    #[tokio::test]
    async fn deploy_run_attach_derives_from_pid_and_states() {
        let base = tmp_base();
        let _g = registry::test_hooks::install_runs_base(base);

        // A run with one confirmed and one rolled-back host.
        let (run_id, dir) = registry::new_run_dir().unwrap();
        let mut meta = Meta::new();
        meta.insert("run_id".into(), Value::from(run_id.clone()));
        meta.insert("limit".into(), Value::from("web,db"));
        meta.insert("dry_activate".into(), Value::from(false));
        meta.insert("pid".into(), Value::from(4242));
        registry::write_meta(&dir, &meta).unwrap();
        write_events(
            &dir.join("web.jsonl"),
            &milestones("web", &["eval", "activate", "confirm"]),
        );
        write_events(
            &dir.join("db.jsonl"),
            &milestones("db", &["eval", "activate", "rollback"]),
        );

        let mut run = DeployRun::attach(&run_id).unwrap();
        assert_eq!(run.limit, "web,db");
        run.poll();

        // pid alive → still running: not finished, no returncode.
        let g = registry::test_hooks::install(|pid| pid == Some(4242));
        assert!(!run.finished());
        assert_eq!(run.returncode(), None);
        drop(g);

        // pid gone → finished; a rolled-back host ⇒ returncode 1.
        let dead = registry::test_hooks::install(|_| false);
        assert!(run.finished());
        assert_eq!(run.returncode(), Some(1));
        run.terminate(); // no-op in attached mode (must not panic / own a child)
        drop(dead);

        // A clean run (all hosts confirmed, pid gone) ⇒ returncode 0.
        let (ok_id, ok_dir) = registry::new_run_dir().unwrap();
        let mut ok_meta = Meta::new();
        ok_meta.insert("run_id".into(), Value::from(ok_id.clone()));
        ok_meta.insert("limit".into(), Value::from("web"));
        ok_meta.insert("pid".into(), Value::from(4243));
        registry::write_meta(&ok_dir, &ok_meta).unwrap();
        write_events(
            &ok_dir.join("web.jsonl"),
            &milestones("web", &["eval", "activate", "confirm"]),
        );
        let mut ok = DeployRun::attach(&ok_id).unwrap();
        ok.poll();
        let _dead = registry::test_hooks::install(|_| false);
        assert!(ok.finished());
        assert_eq!(ok.returncode(), Some(0));

        // A supervisor that died before settling metadata is a failed run,
        // even when no host event had time to arrive.
        let (orphan_id, orphan_dir) = registry::new_run_dir().unwrap();
        let mut orphan_meta = Meta::new();
        orphan_meta.insert("run_id".into(), Value::from(orphan_id.clone()));
        orphan_meta.insert("limit".into(), Value::from("web"));
        orphan_meta.insert("supervisor_pid".into(), Value::from(4244));
        registry::write_meta(&orphan_dir, &orphan_meta).unwrap();
        let mut orphan = DeployRun::attach(&orphan_id).unwrap();
        assert!(orphan.finished());
        assert_eq!(orphan.returncode(), Some(1));
    }
}
