//! The server's effect seam: every subprocess / launch / eval the tools
//! perform goes through the [`Effects`] trait.
//!
//! This is the Rust equivalent of what the Python parity tests monkeypatched
//! (`subprocess.run`, `drift.eval_expected`, `drift.repo_rev`,
//! `runner.DeployRun.start`, `runner.CommandRun`, `shutil.which`): one
//! injectable boundary so the golden-fixture parity tests replay every tool —
//! including the subprocess-dependent ok paths — sandbox-safe, with no
//! ansible/nix/network. [`RealEffects`] is the production implementation over
//! the `mandala-core` runner/drift/eval cores.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mandala_core::drift;
use mandala_core::eval::Evaluator;
use mandala_core::inventory::{Inventory, InventoryError};
use mandala_core::registry::Meta;
use mandala_core::runner::{CommandRun, DeployRun, ansible_dir, reboot_argv};

/// A finished ad-hoc command (ansible ping / systemd restart): captured
/// stdout, stderr, and the exit code (negative = killed by that signal, the
/// Python `Popen.returncode` convention).
#[derive(Debug, Clone)]
pub struct AdhocOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: i64,
}

/// A completed read-only state survey, including the diagnostics needed to
/// distinguish a failed refresh from a successful fresh snapshot set.
#[derive(Debug, Clone)]
pub struct SurveyOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: i64,
}

/// Why an ad-hoc command could not run at all (as opposed to running and
/// failing, which is an [`AdhocOutput`] with a non-zero code).
#[derive(Debug, Clone)]
pub enum AdhocError {
    /// The executable is not on `PATH` (Python's `FileNotFoundError`).
    NotFound,
    /// Any other spawn failure.
    Other(String),
}

/// A structured subordinate-eval failure — the data half of the Python
/// `errors.failure()` shape (`ok`/`error` are added by the tool that owns the
/// summary text). The real evaluator path carries only a message (`output`);
/// the subprocess-era `command`/`exit_code` fields stay in the shape so the
/// result contract never changes.
#[derive(Debug, Clone)]
pub struct EvalFailure {
    pub command: Option<Vec<String>>,
    pub exit_code: Option<i64>,
    pub output: String,
}

/// A launched (or launch-attempted) deploy run's registry identity.
#[derive(Debug, Clone)]
pub struct DeployLaunch {
    pub run_id: String,
    pub events_dir: PathBuf,
}

/// A launched (or launch-attempted) command run's registry identity.
#[derive(Debug, Clone)]
pub struct CommandLaunch {
    pub run_id: String,
    pub log: PathBuf,
    pub launched: bool,
}

/// Every effect the MCP tools perform, behind one injectable boundary.
#[async_trait]
pub trait Effects: Send + Sync {
    /// Evaluate a fresh inventory aggregate for `flake` — the lazy first read
    /// AND the `reload` tool's swap both come through here. The real
    /// implementation re-roots the eval worker first (warm state must never
    /// serve a moved contract) and honours the `MANDALA_AGGREGATE_FILE` test
    /// seam the CLI shares.
    async fn fresh_inventory(&self, flake: &str) -> Result<Inventory, InventoryError>;

    /// Expected toplevel out-paths for `members` (the slow eval behind
    /// `host_eval toplevel=true` and `drift do_eval=true`). Failures are
    /// returned structured, never raised to the transport.
    async fn eval_expected(
        &self,
        flake: &str,
        members: &[String],
    ) -> Result<BTreeMap<String, String>, EvalFailure>;

    /// The contract's git rev (`-dirty`-suffixed), `None` on any git failure.
    async fn repo_rev(&self, flake: &str) -> Option<String>;

    /// Run the read-only state survey playbook, capturing its complete result.
    async fn refresh_snapshots(&self) -> io::Result<SurveyOutput>;

    /// Run an ad-hoc argv (ansible ping / systemd restart) in the shared
    /// ansible working directory with deprecation chatter silenced, capturing
    /// stdout + stderr separately.
    async fn run_adhoc(&self, argv: Vec<String>) -> Result<AdhocOutput, AdhocError>;

    /// Launch the native deploy engine and attach its engine-owned registry run.
    async fn launch_deploy(
        &self,
        flake: &str,
        limit: &str,
        dry_activate: bool,
        throttle: i64,
    ) -> io::Result<DeployLaunch>;

    /// Launch a registered background command run (build / reboot) into the
    /// shared registry, output teed to its `output.log`.
    async fn launch_command(
        &self,
        argv: Vec<String>,
        kind: &str,
        cwd: Option<PathBuf>,
        extra_meta: Meta,
    ) -> io::Result<CommandLaunch>;

    /// The reboot launch line (`ans-reboot` wrapper preferred, playbook
    /// fallback), `None` when neither exists.
    fn reboot_argv(&self, target: &str, serial: &str, drain: bool) -> Option<Vec<String>>;
}

/// The production [`Effects`]: `mandala-core`'s evaluator, drift helpers, and
/// tokio runners. Blocking work (evals, git, the survey playbook) runs on the
/// blocking pool so a slow eval never wedges the server's message loop.
pub struct RealEffects {
    evaluator: Arc<Mutex<Evaluator>>,
}

impl RealEffects {
    #[must_use]
    pub fn new() -> Self {
        Self {
            evaluator: Arc::new(Mutex::new(Evaluator::from_env())),
        }
    }

    /// Production effects whose children never write through to the
    /// terminal: a quiet evaluator worker and an output-nulled survey. The
    /// TUI's context factory uses this; the stdio server keeps [`Self::new`]
    /// (its stderr is free, and errors travel in-band either way).
    #[must_use]
    pub fn quiet() -> Self {
        Self {
            evaluator: Arc::new(Mutex::new(Evaluator::from_env().quiet())),
        }
    }
}

impl Default for RealEffects {
    fn default() -> Self {
        Self::new()
    }
}

/// Map an exit status to the Python `Popen.returncode` convention.
fn status_code(status: std::process::ExitStatus) -> i64 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .map_or_else(|| i64::from(-status.signal().unwrap_or(0)), i64::from)
}

#[async_trait]
impl Effects for RealEffects {
    async fn fresh_inventory(&self, flake: &str) -> Result<Inventory, InventoryError> {
        // The CLI's `MANDALA_AGGREGATE_FILE` test seam, honoured here too so
        // the stdio integration test injects a fixture aggregate without a
        // flake eval (parity with `mandala_core::cli::load_inventory`).
        if let Ok(path) = std::env::var("MANDALA_AGGREGATE_FILE")
            && !path.is_empty()
        {
            let text = std::fs::read_to_string(&path).map_err(|e| {
                InventoryError::Eval(format!("reading MANDALA_AGGREGATE_FILE {path}: {e}"))
            })?;
            let value: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| InventoryError::Eval(format!("parsing {path}: {e}")))?;
            return Inventory::from_value(value);
        }
        let evaluator = Arc::clone(&self.evaluator);
        let flake = flake.to_string();
        tokio::task::spawn_blocking(move || {
            let mut guard = evaluator
                .lock()
                .map_err(|_| InventoryError::Eval("evaluator lock poisoned".to_string()))?;
            // Re-root the warm worker first: a stale EvalState must never
            // serve a moved contract (`mem:mandala/mcp` live-inventory pass).
            guard.reload().map_err(InventoryError::Eval)?;
            Inventory::from_evaluator(&mut guard, &flake)
        })
        .await
        .map_err(|e| InventoryError::Eval(format!("eval task failed: {e}")))?
    }

    async fn eval_expected(
        &self,
        flake: &str,
        members: &[String],
    ) -> Result<BTreeMap<String, String>, EvalFailure> {
        let evaluator = Arc::clone(&self.evaluator);
        let flake = flake.to_string();
        let members = members.to_vec();
        let joined = tokio::task::spawn_blocking(move || {
            let mut guard = evaluator
                .lock()
                .map_err(|_| drift::DriftError::Eval("evaluator lock poisoned".to_string()))?;
            drift::eval_expected(&mut guard, &flake, &members)
        })
        .await;
        match joined {
            Ok(Ok(map)) => Ok(map),
            Ok(Err(e)) => Err(EvalFailure {
                command: None,
                exit_code: None,
                output: e.to_string(),
            }),
            Err(e) => Err(EvalFailure {
                command: None,
                exit_code: None,
                output: format!("eval task failed: {e}"),
            }),
        }
    }

    async fn repo_rev(&self, flake: &str) -> Option<String> {
        let flake = flake.to_string();
        tokio::task::spawn_blocking(move || drift::repo_rev(&flake))
            .await
            .ok()
            .flatten()
    }

    async fn refresh_snapshots(&self) -> io::Result<SurveyOutput> {
        // `output()` captures both streams for MCP diagnostics and also keeps
        // the TUI leader's alternate screen quiet.
        let output = tokio::process::Command::new("ansible-playbook")
            .arg("mandala.fleet.state")
            .current_dir(ansible_dir())
            .env("MANDALA_FLEET_STATE", drift::state_dir())
            .stdin(Stdio::null())
            .output()
            .await?;
        Ok(SurveyOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            code: status_code(output.status),
        })
    }

    async fn run_adhoc(&self, argv: Vec<String>) -> Result<AdhocOutput, AdhocError> {
        let mut cmd = tokio::process::Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(ansible_dir())
            // Silence deprecation chatter that would otherwise ride along in
            // every tool result (the Python `_adhoc_env`).
            .env("ANSIBLE_DEPRECATION_WARNINGS", "False")
            // Never inherit the server's stdin — it is the MCP transport.
            .stdin(Stdio::null());
        match cmd.output().await {
            Ok(out) => Ok(AdhocOutput {
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                code: status_code(out.status),
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(AdhocError::NotFound),
            Err(e) => Err(AdhocError::Other(e.to_string())),
        }
    }

    async fn launch_deploy(
        &self,
        flake: &str,
        limit: &str,
        dry_activate: bool,
        throttle: i64,
    ) -> io::Result<DeployLaunch> {
        let mut run = DeployRun::new(limit);
        run.flake = flake.to_string();
        run.dry_activate = dry_activate;
        run.throttle = throttle;
        run.start().await?;
        loop {
            match run.discover_run() {
                Ok(true) => break,
                Ok(false) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
                Err(error) => return Err(error),
            }
        }
        let run_id = run
            .run_id
            .clone()
            .ok_or_else(|| io::Error::other("native deploy attached without a run id"))?;
        let events_dir = run
            .events_dir
            .clone()
            .ok_or_else(|| io::Error::other("native deploy attached without a registry path"))?;
        Ok(DeployLaunch { run_id, events_dir })
    }

    async fn launch_command(
        &self,
        argv: Vec<String>,
        kind: &str,
        cwd: Option<PathBuf>,
        extra_meta: Meta,
    ) -> io::Result<CommandLaunch> {
        let mut run = CommandRun::new(argv, kind);
        run.cwd = cwd;
        run.extra_meta = extra_meta;
        run.start().await?;
        Ok(CommandLaunch {
            run_id: run.run_id.clone().unwrap_or_default(),
            log: run.log_path().unwrap_or_default(),
            launched: run.launched(),
        })
    }

    fn reboot_argv(&self, target: &str, serial: &str, drain: bool) -> Option<Vec<String>> {
        reboot_argv(target, serial, drain)
    }
}
