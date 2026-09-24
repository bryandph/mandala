//! Built-in `deploy` engine: native deploy orchestration plus the retained
//! cache-warming and node-listing views.
//!
//! `deploy run` consumes the already-evaluated aggregate supplied by the CLI,
//! resolves its selection, and creates an always-discoverable registry run.
//! The native batch build extends that prepared run, then a throttle-bounded
//! JoinSet activates each prebuilt profile independently and settles the run
//! from sticky host outcomes. `batch` remains the explicit cache-warming build
//! and `nodes` lists the projected deploy-rs nodes.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::fmt;
use std::future::Future;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Command as ProcCommand, ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};
use mandala_deploy::data::{GenericSettings, Node, NodeSettings, Profile, ProfileSettings};
use mandala_deploy::deploy::{
    ActivationDisposition, deploy_profile, resolve_activation_disposition, stage_profile_for_boot,
};
use mandala_deploy::push::push_profile;
use mandala_deploy::{CmdOverrides, EventSink, Level, make_deploy_data};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::cli::Engine;
use crate::inventory::{Inventory, InventoryError};
use crate::registry::{self, Meta};
use crate::runner::{EventWriter, HostState};

/// The `deploy` engine registration (name + subcommand tree + handler).
#[must_use]
pub fn engine() -> Engine {
    let command = Command::new("deploy")
        .about("Native eval-once fan-out deploys via deploy-rs")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("run")
                .about("Run the native eval-once + fan-out deploy")
                .arg(
                    Arg::new("limit")
                        .short('l')
                        .long("limit")
                        .required(true)
                        .help("Selector: @group (expanded to the projected members), member, or comma-list"),
                )
                .arg(
                    Arg::new("dry-activate")
                        .long("dry-activate")
                        .action(ArgAction::SetTrue)
                        .help("Build + copy but do not activate"),
                )
                .arg(
                    Arg::new("boot")
                        .long("boot")
                        .action(ArgAction::SetTrue)
                        .help("Use boot activation for every selected member"),
                )
                .arg(
                    Arg::new("halt-on-build-failure")
                        .long("halt-on-build-failure")
                        .action(ArgAction::SetTrue)
                        .help("Stop on a build failure without deploying any targets"),
                )
                .arg(
                    Arg::new("throttle")
                        .long("throttle")
                        .value_parser(value_parser!(i64))
                        .default_value("4")
                        .help("Per-host deploy parallelism"),
                ),
        )
        .subcommand(
            Command::new("batch")
                .about("Build a group's eval-once batch artifact (.#deployBatch.<group>)")
                .arg(
                    Arg::new("group")
                        .required(true)
                        .help("deployBatch group key (taxonomy spelling)"),
                ),
        )
        .subcommand(
            Command::new("nodes").about("List deploy-rs node names (from the aggregate's deploy projection)"),
        );
    Engine::new(command, run)
}

/// A native deploy's immutable preflight result. Settings are copied directly
/// from `projections.deploy.settings`; no deploy setting is re-merged in Rust.
#[derive(Debug, Clone, PartialEq)]
pub struct DeployPlan {
    /// Canonical `to_limit` spelling of the complete operator selection.
    pub limit: String,
    /// Deployable members, sorted by the inventory selector algebra.
    pub targets: Vec<String>,
    /// Selected members whose `deployment.deployRs.enable` is false.
    pub skipped: Vec<String>,
    /// Flattened settings for exactly [`Self::targets`].
    pub settings: BTreeMap<String, Value>,
    /// Whether activation is suppressed after copy.
    pub dry_activate: bool,
    /// Whether every selected member uses boot activation for this run.
    pub boot: bool,
    /// Opt-in fail-fast build policy; defaults to best effort.
    pub halt_on_build_failure: bool,
    /// Maximum per-host concurrency for the later fan-out stage.
    pub throttle: i64,
}

/// A prepared native deploy after its discoverable registry files exist.
#[derive(Debug, Clone)]
pub struct RegisteredDeployRun {
    /// The registry run id.
    pub run_id: String,
    /// The registry run directory.
    pub path: PathBuf,
    /// The preflight plan carried into build and activation stages.
    pub plan: DeployPlan,
}

/// The selected profiles produced by the one native batch-build invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltProfiles {
    /// Deploy target -> prebuilt profile store path.
    pub paths: BTreeMap<String, PathBuf>,
    /// Targets whose profiles could not be built; never passed to deployment.
    pub failures: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ActivationMode {
    Switch,
    Boot,
}

/// The Stage-A flattened setting shape. Required fields are the projection's
/// execution contract; nullable fields stay optional and are passed through
/// without another merge.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FlattenedDeploySettings {
    activation: ActivationMode,
    hostname: String,
    ssh_user: String,
    ssh_port: u16,
    identity_file: Option<PathBuf>,
    #[serde(default)]
    ssh_opts: Vec<String>,
    auto_rollback: bool,
    fast_connection: bool,
    magic_rollback: Option<bool>,
    confirm_timeout: Option<u16>,
    activation_timeout: Option<u16>,
    temp_path: Option<PathBuf>,
    sudo: Option<String>,
    user: Option<String>,
}

impl FlattenedDeploySettings {
    fn generic(&self) -> GenericSettings {
        GenericSettings {
            ssh_user: Some(self.ssh_user.clone()),
            ssh_port: Some(self.ssh_port),
            identity_file: self.identity_file.clone(),
            user: self.user.clone(),
            ssh_opts: self.ssh_opts.clone(),
            fast_connection: Some(self.fast_connection),
            auto_rollback: Some(self.auto_rollback),
            confirm_timeout: self.confirm_timeout,
            activation_timeout: self.activation_timeout,
            temp_path: self.temp_path.clone(),
            magic_rollback: self.magic_rollback,
            sudo: self.sudo.clone(),
            remote_build: None,
            interactive_sudo: None,
        }
    }
}

/// One completed native per-host task. The state uses the same sticky
/// vocabulary consumed by registry readers and frontends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDeployResult {
    pub host: String,
    pub state: HostState,
    pub error: Option<String>,
}

/// Deterministic whole-run counts persisted in `meta.json` and printed by the
/// headless CLI after every completed native fan-out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeploySummary {
    pub total: usize,
    pub confirmed: usize,
    pub reboot_pending: usize,
    pub failed: usize,
    pub rolled_back: usize,
}

/// A fully settled native fan-out. `process_rc` is the raw controller result;
/// `rc` is the effective fleet result after sticky host outcomes are applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployOutcome {
    pub results: BTreeMap<String, HostDeployResult>,
    pub summary: DeploySummary,
    pub process_rc: i32,
    pub rc: i32,
}

type HostTaskFuture = Pin<Box<dyn Future<Output = HostDeployResult> + Send + 'static>>;
type HostTask = Arc<
    dyn Fn(Arc<RegisteredDeployRun>, Arc<BuiltProfiles>, String) -> HostTaskFuture + Send + Sync,
>;

#[derive(Debug, Clone, Default)]
struct DeployPrograms {
    nix: Option<PathBuf>,
    ssh: Option<PathBuf>,
    environment: Vec<(String, String)>,
}

struct RegistryDeploySink {
    writer: Arc<EventWriter>,
    error: Mutex<Option<String>>,
}

impl RegistryDeploySink {
    fn new(writer: Arc<EventWriter>) -> Self {
        Self {
            writer,
            error: Mutex::new(None),
        }
    }

    fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|error| error.clone())
    }
}

impl EventSink for RegistryDeploySink {
    fn emit(&self, _level: Level, message: &str) {
        let result = self.writer.emit(
            "line",
            event_fields([
                ("line", Value::from(message)),
                ("stream", Value::from("deploy")),
            ]),
        );
        if let Err(error) = result
            && let Ok(mut slot) = self.error.lock()
            && slot.is_none()
        {
            *slot = Some(error.to_string());
        }
    }
}

/// A native batch-build failure. A non-zero Nix exit is distinct from an I/O
/// or output-mapping failure so the CLI preserves the real process rc.
#[derive(Debug)]
pub enum BuildError {
    /// The Nix process ran and failed.
    Failed(i32),
    /// Spawning, streaming, registry emission, or path mapping failed.
    Io(io::Error),
    /// Nix succeeded but did not leave the expected indexed profile link.
    MissingProfileLink { host: String, path: PathBuf },
    /// An indexed link did not resolve to an absolute Nix store path.
    InvalidProfilePath { host: String, path: PathBuf },
}

impl BuildError {
    fn exit_code(&self) -> i32 {
        match self {
            Self::Failed(rc) => *rc,
            Self::Io(_) | Self::MissingProfileLink { .. } | Self::InvalidProfilePath { .. } => 1,
        }
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed(rc) => write!(f, "nix build failed (rc={rc})"),
            Self::Io(err) => write!(f, "running nix build: {err}"),
            Self::MissingProfileLink { host, path } => {
                write!(
                    f,
                    "nix build produced no profile link for {host}: {}",
                    path.display()
                )
            }
            Self::InvalidProfilePath { host, path } => write!(
                f,
                "nix build produced an invalid profile path for {host}: {}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for BuildError {}

impl From<io::Error> for BuildError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Errors that must stop native deploy orchestration before effects advance.
#[derive(Debug)]
pub enum DeployRunError {
    /// Selector resolution failed.
    Inventory(InventoryError),
    /// No selected member has deploy-rs enabled.
    NoDeployableMembers(String),
    /// A semaphore cannot enforce a non-positive concurrency bound.
    InvalidThrottle(i64),
    /// The aggregate lacks the Stage-A flattened-settings projection.
    MissingSettingsProjection,
    /// An enabled member lacks its required flattened settings entry.
    MissingMemberSettings(String),
    /// Creating the run registry failed.
    Registry(io::Error),
}

impl fmt::Display for DeployRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inventory(err) => err.fmt(f),
            Self::NoDeployableMembers(limit) => {
                write!(f, "selection has no deploy-rs members: {limit}")
            }
            Self::InvalidThrottle(throttle) => {
                write!(f, "deploy throttle must be positive (got {throttle})")
            }
            Self::MissingSettingsProjection => {
                write!(f, "aggregate has no projections.deploy.settings object")
            }
            Self::MissingMemberSettings(host) => {
                write!(f, "aggregate has no flattened deploy settings for {host}")
            }
            Self::Registry(err) => write!(f, "creating deploy registry run: {err}"),
        }
    }
}

impl std::error::Error for DeployRunError {}

impl From<InventoryError> for DeployRunError {
    fn from(value: InventoryError) -> Self {
        Self::Inventory(value)
    }
}

impl From<io::Error> for DeployRunError {
    fn from(value: io::Error) -> Self {
        Self::Registry(value)
    }
}

fn deploy_rs_enabled(inv: &Inventory, host: &str) -> bool {
    inv.members()
        .get(host)
        .and_then(|member| member.get("deployment"))
        .and_then(|deployment| deployment.get("deployRs"))
        .and_then(|deploy_rs| deploy_rs.get("enable"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Resolve and validate a native deploy before a registry run is allocated.
///
/// This ordering is deliberate: a bad selector or selection with no
/// deployable members leaves no empty run behind. The [`Inventory`] itself was
/// built by the CLI's one aggregate evaluation, so consuming the flattened
/// settings here does not perform another eval.
///
/// # Errors
/// Selector errors, a zero-deployable selection, or a missing/malformed
/// flattened-settings projection.
pub fn plan_run(
    inv: &Inventory,
    limit: &str,
    throttle: i64,
    dry_activate: bool,
) -> Result<DeployPlan, DeployRunError> {
    plan_run_with_boot(inv, limit, throttle, dry_activate, false)
}

/// Resolve and validate a native deploy with an invocation-time boot override.
///
/// # Errors
/// Selector errors, a zero-deployable selection, or a missing/malformed
/// flattened-settings projection.
pub fn plan_run_with_boot(
    inv: &Inventory,
    limit: &str,
    throttle: i64,
    dry_activate: bool,
    boot: bool,
) -> Result<DeployPlan, DeployRunError> {
    if throttle <= 0 {
        return Err(DeployRunError::InvalidThrottle(throttle));
    }
    let canonical_limit = inv.to_limit(limit)?;
    let selected: Vec<String> = canonical_limit.split(',').map(str::to_string).collect();
    let (targets, skipped): (Vec<_>, Vec<_>) = selected
        .into_iter()
        .partition(|host| deploy_rs_enabled(inv, host));

    if targets.is_empty() {
        return Err(DeployRunError::NoDeployableMembers(canonical_limit));
    }

    let projected = inv
        .deploy_settings()
        .ok_or(DeployRunError::MissingSettingsProjection)?;
    let mut settings = BTreeMap::new();
    for host in &targets {
        let value = projected
            .get(host)
            .ok_or_else(|| DeployRunError::MissingMemberSettings(host.clone()))?;
        settings.insert(host.clone(), value.clone());
    }

    Ok(DeployPlan {
        limit: canonical_limit,
        targets,
        skipped,
        settings,
        dry_activate,
        boot,
        halt_on_build_failure: false,
        throttle,
    })
}

fn now_epoch_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

/// Allocate and initialize the registry entry for a validated deploy plan.
/// One stream is created per deployable host before the plan is handed to the
/// build/fan-out stages.
///
/// # Errors
/// Any registry directory, stream, or metadata write failure.
pub fn register_run(plan: DeployPlan) -> Result<RegisteredDeployRun, DeployRunError> {
    let (run_id, path) = registry::new_run_dir()?;
    for host in &plan.targets {
        std::fs::File::create(path.join(format!("{host}.jsonl")))?;
    }

    let mut meta = Meta::new();
    meta.insert("run_id".into(), Value::from(run_id.clone()));
    meta.insert("kind".into(), Value::from("deploy"));
    meta.insert("limit".into(), Value::from(plan.limit.clone()));
    meta.insert("targets".into(), Value::from(plan.targets.clone()));
    meta.insert("skipped".into(), Value::from(plan.skipped.clone()));
    meta.insert("dry_activate".into(), Value::from(plan.dry_activate));
    meta.insert("boot".into(), Value::from(plan.boot));
    meta.insert("throttle".into(), Value::from(plan.throttle));
    meta.insert(
        "halt_on_build_failure".into(),
        Value::from(plan.halt_on_build_failure),
    );
    meta.insert("pid".into(), Value::from(i64::from(std::process::id())));
    meta.insert("started_at".into(), Value::from(now_epoch_f64()));
    registry::write_meta(&path, &meta)?;

    Ok(RegisteredDeployRun { run_id, path, plan })
}

/// Perform native deploy preflight and create its registry run.
///
/// Keeping plan validation ahead of [`register_run`] is what guarantees that
/// invalid and zero-deployable selections leave the registry untouched.
///
/// # Errors
/// Any error from [`plan_run`] or [`register_run`].
pub fn prepare_run(
    inv: &Inventory,
    limit: &str,
    throttle: i64,
    dry_activate: bool,
) -> Result<RegisteredDeployRun, DeployRunError> {
    prepare_run_with_boot(inv, limit, throttle, dry_activate, false)
}

/// Perform native deploy preflight with an invocation-time boot override and
/// create its registry run.
///
/// # Errors
/// Any error from [`plan_run_with_boot`] or [`register_run`].
pub fn prepare_run_with_boot(
    inv: &Inventory,
    limit: &str,
    throttle: i64,
    dry_activate: bool,
    boot: bool,
) -> Result<RegisteredDeployRun, DeployRunError> {
    register_run(plan_run_with_boot(
        inv,
        limit,
        throttle,
        dry_activate,
        boot,
    )?)
}

const NIX_JSON_PREFIX: &str = "@nix ";
const ACT_COPY_PATH: i64 = 100;
const ACT_COPY_PATHS: i64 = 103;
const ACT_BUILD: i64 = 105;

#[derive(Debug, Default)]
struct BuildTracker {
    built: i64,
    finished: i64,
    fetched: i64,
    fetched_done: i64,
    errors: i64,
    current: String,
    running: BTreeMap<i64, String>,
    error_messages: Vec<String>,
}

impl BuildTracker {
    fn feed(&mut self, line: &str) -> bool {
        let Some(payload) = line.strip_prefix(NIX_JSON_PREFIX) else {
            return false;
        };
        let Ok(event) = serde_json::from_str::<Value>(payload) else {
            return false;
        };
        match event.get("action").and_then(Value::as_str) {
            Some("start") => {
                let event_type = event.get("type").and_then(Value::as_i64);
                let id = event.get("id").and_then(Value::as_i64);
                if event_type == Some(ACT_BUILD) {
                    let Some(id) = id else { return false };
                    let derivation = event
                        .get("fields")
                        .and_then(Value::as_array)
                        .and_then(|fields| fields.first())
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let name = derivation_name(derivation);
                    self.current = name.clone();
                    self.running.insert(id, name);
                    self.built += 1;
                    true
                } else if matches!(event_type, Some(ACT_COPY_PATH | ACT_COPY_PATHS)) {
                    self.fetched += 1;
                    true
                } else {
                    false
                }
            }
            Some("stop") => {
                let id = event.get("id").and_then(Value::as_i64);
                if id.and_then(|id| self.running.remove(&id)).is_some() {
                    self.finished += 1;
                    if let Some(last) = self.running.values().next() {
                        self.current.clone_from(last);
                    }
                    true
                } else if self.fetched_done < self.fetched {
                    self.fetched_done += 1;
                    true
                } else {
                    false
                }
            }
            Some("msg") if event.get("level").and_then(Value::as_i64) == Some(0) => {
                let message = event
                    .get("raw_msg")
                    .or_else(|| event.get("msg"))
                    .or_else(|| event.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !message.is_empty() {
                    self.errors += 1;
                    self.error_messages.push(message.to_string());
                }
                false
            }
            _ => false,
        }
    }

    fn fields(&self) -> serde_json::Map<String, Value> {
        [
            ("built", Value::from(self.built)),
            ("finished", Value::from(self.finished)),
            ("fetched", Value::from(self.fetched)),
            ("fetched_done", Value::from(self.fetched_done)),
            ("errors", Value::from(self.errors)),
            ("current", Value::from(self.current.clone())),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
    }
}

fn derivation_name(path: &str) -> String {
    let base = path
        .rsplit_once('/')
        .map_or(path, |(_, basename)| basename)
        .strip_suffix(".drv")
        .unwrap_or_else(|| path.rsplit_once('/').map_or(path, |(_, basename)| basename));
    match base.split_once('-') {
        Some((hash, name)) if hash.len() == 32 => name.to_string(),
        _ => base.to_string(),
    }
}

/// The single targeted Nix build invocation for a native deploy. Installables
/// are ordered exactly like `targets`; Nix's documented multi-result out-link
/// indices then provide the deterministic target -> store-path mapping.
#[must_use]
pub fn build_run_argv(flake: &str, targets: &[String], out_link: &Path) -> Vec<String> {
    let mut argv = vec!["nix".to_string(), "build".to_string()];
    argv.extend(
        targets
            .iter()
            .map(|host| format!("{flake}#deploy.nodes.{host}.profiles.system.path")),
    );
    argv.extend([
        "--keep-going".to_string(),
        "--log-format".to_string(),
        "internal-json".to_string(),
        "--impure".to_string(),
        "--out-link".to_string(),
        out_link.display().to_string(),
    ]);
    argv
}

fn profile_link(out_link: &Path, index: usize) -> PathBuf {
    if index == 0 {
        return out_link.to_path_buf();
    }
    let filename = out_link
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("profile");
    out_link.with_file_name(format!("{filename}-{index}"))
}

fn map_profile_links(
    targets: &[String],
    out_link: &Path,
) -> Result<BTreeMap<String, PathBuf>, BuildError> {
    let mut paths = BTreeMap::new();
    for (index, host) in targets.iter().enumerate() {
        let link = profile_link(out_link, index);
        let path = std::fs::read_link(&link).map_err(|_| BuildError::MissingProfileLink {
            host: host.clone(),
            path: link,
        })?;
        if !path.is_absolute() || !path.starts_with("/nix/store") {
            return Err(BuildError::InvalidProfilePath {
                host: host.clone(),
                path,
            });
        }
        paths.insert(host.clone(), path);
    }
    Ok(paths)
}

/// Resolve once, before effects, so recovery never re-evaluates a changing flake.
/// Nix's dry-run JSON retains installable order, including shared derivations.
fn resolve_profiles(
    program: &OsStr,
    argv: &[String],
    count: usize,
) -> Result<Vec<(String, String)>, BuildError> {
    let output = ProcCommand::new(program)
        .args(&argv[1..])
        .args(["--dry-run", "--json"])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "resolving profiles: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }
    let values: Vec<Value> = serde_json::from_slice(&output.stdout).map_err(io::Error::other)?;
    if values.len() != count {
        return Err(
            io::Error::other("profile resolution returned an unexpected target count").into(),
        );
    }
    values
        .iter()
        .map(|value| {
            let drv = value["drvPath"].as_str();
            let outputs = value["outputs"].as_object();
            match (drv, outputs) {
                (Some(drv), Some(outputs))
                    if drv.starts_with("/nix/store/")
                        && drv.ends_with(".drv")
                        && outputs.len() == 1 =>
                {
                    let (name, path) = outputs.iter().next().expect("one output");
                    let path = path
                        .as_str()
                        .filter(|p| p.starts_with("/nix/store/"))
                        .ok_or_else(|| {
                            io::Error::other("profile has no statically known store output")
                        })?;
                    Ok((format!("{drv}^{name}"), path.to_string()))
                }
                _ => Err(io::Error::other(
                    "profile resolution requires one derivation output per target",
                )
                .into()),
            }
        })
        .collect()
}

/// Probe exact outputs with all building and substitution disabled. Successful
/// probes also create GC roots; failed builds cannot accidentally be retried.
fn recover_profiles(
    run: &RegisteredDeployRun,
    program: &OsStr,
    resolved: &[(String, String)],
    rc: i32,
) -> Result<BuiltProfiles, BuildError> {
    let mut built = BuiltProfiles {
        paths: BTreeMap::new(),
        failures: BTreeMap::new(),
    };
    for (index, (host, (drv, path))) in run.plan.targets.iter().zip(resolved).enumerate() {
        let link = profile_link(&run.path.join("profile"), index);
        let output = ProcCommand::new(program)
            .args([
                "build",
                path,
                "--offline",
                "--max-jobs",
                "0",
                "--builders",
                "",
                "--option",
                "substitute",
                "false",
                "--out-link",
            ])
            .arg(&link)
            .output()?;
        if output.status.success() {
            let mapped = map_profile_links(std::slice::from_ref(host), &link)?;
            if mapped[host] != Path::new(path) {
                return Err(BuildError::InvalidProfilePath {
                    host: host.clone(),
                    path: mapped[host].clone(),
                });
            }
            built.paths.extend(mapped);
        } else {
            built.failures.insert(host.clone(), format!("profile build failed (batch rc={rc}): {drv}; see build.jsonl for dependency errors and derivation logs"));
        }
    }
    Ok(built)
}

fn save_derivation_log(
    program: &OsStr,
    run: &RegisteredDeployRun,
    drv: &str,
) -> io::Result<PathBuf> {
    let name = Path::new(drv)
        .file_name()
        .ok_or_else(|| io::Error::other("derivation has no filename"))?;
    let directory = run.path.join("build-logs");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(name).with_extension("log");
    let file = std::fs::File::create(&path)?;
    let output = ProcCommand::new(program)
        .args(["log", "--offline", drv])
        .stdout(file)
        .stderr(Stdio::piped())
        .output()?;
    if output.status.success() {
        Ok(path)
    } else {
        let _ = std::fs::remove_file(&path);
        Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}

fn event_fields<const N: usize>(fields: [(&str, Value); N]) -> serde_json::Map<String, Value> {
    fields
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

fn record_build_failure(run: &RegisteredDeployRun, rc: i32, error: &str) {
    let mut fields = Meta::new();
    fields.insert("build_rc".into(), Value::from(rc));
    fields.insert("rc".into(), Value::from(rc));
    fields.insert("error".into(), Value::from(error));
    fields.insert("finished_at".into(), Value::from(now_epoch_f64()));
    let _ = registry::update_meta(&run.path, fields);
}

/// Resolve selected profiles once, build them together with keep-going, and
/// return verified successes alongside per-host build failures. Registry events
/// retain Nix's process result and derivation diagnostics.
///
/// # Errors
/// Resolution, cancellation, process/event I/O, or invalid output mapping.
/// Ordinary build failures are returned as per-host outcomes for fan-out.
pub fn build_profiles(run: &RegisteredDeployRun, flake: &str) -> Result<BuiltProfiles, BuildError> {
    build_profiles_with(run, flake, OsStr::new("nix"))
}

fn build_profiles_with(
    run: &RegisteredDeployRun,
    flake: &str,
    program: &OsStr,
) -> Result<BuiltProfiles, BuildError> {
    let out_link = run.path.join("profile");
    let mut duration_cache = crate::runner::build_duration_cache_path(&run.path).and_then(|path| {
        match nix_build_forest::DurationCache::load(&path) {
            Ok(cache) => Some((path, cache)),
            Err(error) => {
                eprintln!("mandala: build duration cache ignored: {error}");
                None
            }
        }
    });
    let duration_estimates = duration_cache
        .as_ref()
        .map_or_else(BTreeMap::new, |(_, cache)| cache.estimates_ms());
    let mut argv = build_run_argv(flake, &run.plan.targets, &out_link);
    if run.plan.halt_on_build_failure {
        argv.retain(|arg| arg != "--keep-going");
        argv.extend(["--option".into(), "keep-going".into(), "false".into()]);
    }
    let resolved = match resolve_profiles(program, &argv, run.plan.targets.len()) {
        Ok(resolved) => resolved,
        Err(error) => {
            record_build_failure(run, error.exit_code(), &error.to_string());
            return Err(error);
        }
    };
    for (arg, (drv, _)) in argv[2..].iter_mut().zip(&resolved) {
        arg.clone_from(drv);
    }
    let mut event_argv = argv.clone();
    event_argv[0] = program.to_string_lossy().into_owned();
    let writer = Arc::new(EventWriter::new(&run.path, "build", "controller", "build")?);
    if let Err(error) = writer.emit(
        "status",
        event_fields([
            ("cmd", Value::from(event_argv.clone())),
            ("state", Value::from("start")),
        ]),
    ) {
        record_build_failure(run, 1, &error.to_string());
        return Err(error.into());
    }

    eprintln!("mandala: build: {}", event_argv.join(" "));
    let mut command = ProcCommand::new(program);
    command
        .args(&argv[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = writer.emit(
                "status",
                event_fields([("rc", Value::from(127)), ("state", Value::from("done"))]),
            );
            record_build_failure(run, 127, &error.to_string());
            return Err(error.into());
        }
    };

    let stdout = child.stdout.take().expect("piped stdout");
    let stdout_task = std::thread::spawn(move || -> io::Result<Vec<String>> {
        BufReader::new(stdout).lines().collect()
    });
    let stderr = child.stderr.take().expect("piped stderr");
    let stream_writer = Arc::clone(&writer);
    let stderr_task = std::thread::spawn(move || -> (BuildTracker, nix_build_forest::BuildForest, Option<io::Error>) {
        let mut tracker = BuildTracker::default();
        let mut forest =
            nix_build_forest::BuildForest::with_duration_estimates(duration_estimates);
        let mut event_error = None;
        for line in BufReader::new(stderr).lines() {
            let line = match line {
                Ok(line) => line,
                Err(error) => {
                    event_error.get_or_insert(error);
                    break;
                }
            };
            if line.starts_with(NIX_JSON_PREFIX) {
                let log_sequence = forest.log_sequence();
                forest.feed_line(&line);
                if forest.log_sequence() != log_sequence
                    && let Some(log) = forest.latest_log()
                {
                    eprintln!(
                        "mandala: nix: {}",
                        nix_build_forest::plain::render_log(log)
                    );
                }
                if event_error.is_none()
                    && let Err(error) = stream_writer.emit(
                        "nixlog",
                        event_fields([("line", Value::from(line.clone()))]),
                    )
                {
                    event_error = Some(error);
                }
                let changed = tracker.feed(&line);
                if changed {
                    eprintln!(
                        "mandala: {}",
                        nix_build_forest::plain::render_live(&forest.snapshot())
                    );
                }
                if changed
                    && event_error.is_none()
                    && let Err(error) = stream_writer.emit("progress", tracker.fields())
                {
                    event_error = Some(error);
                }
            } else if !line.is_empty() {
                eprintln!("mandala: nix: {line}");
                if event_error.is_none()
                    && let Err(error) = stream_writer.emit(
                        "line",
                        event_fields([("line", Value::from(line)), ("stream", Value::from("nix"))]),
                    )
                {
                    event_error = Some(error);
                }
            }
        }
        // Graph I/O stays off the process/UI event path. A final recursive
        // backfill makes the no-TTY summary and persisted snapshot aware of
        // derivations Nix did not build because they were already valid.
        if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            runtime.block_on(forest.resolve_pending(&nix_build_forest::FsDrvReader));
        }
        (tracker, forest, event_error)
    });

    let status = child.wait()?;
    let stdout_lines = stdout_task
        .join()
        .map_err(|_| io::Error::other("nix stdout reader panicked"))??;
    for line in stdout_lines.into_iter().filter(|line| !line.is_empty()) {
        eprintln!("mandala: nix: {line}");
    }
    let (tracker, forest, event_error) = stderr_task
        .join()
        .map_err(|_| io::Error::other("nix stderr reader panicked"))?;
    for message in &tracker.error_messages {
        eprintln!("mandala: nix error: {message}");
    }

    let process_rc = status.code().unwrap_or(1);
    let outcome = if let Some(error) = event_error {
        Err(BuildError::Io(error))
    } else if status.code().is_none()
        || process_rc == 130
        || process_rc == 143
        || (process_rc != 0 && run.plan.halt_on_build_failure)
    {
        Err(BuildError::Failed(process_rc))
    } else if process_rc != 0 {
        recover_profiles(run, program, &resolved, process_rc)
    } else {
        map_profile_links(&run.plan.targets, &out_link).map(|paths| BuiltProfiles {
            paths,
            failures: BTreeMap::new(),
        })
    };
    let rc = outcome
        .as_ref()
        .map_or_else(|error| error.exit_code(), |_| process_rc);
    if let Err(error) = writer.emit("progress", tracker.fields()).and_then(|()| {
        writer.emit(
            "status",
            event_fields([("rc", Value::from(rc)), ("state", Value::from("done"))]),
        )
    }) {
        record_build_failure(run, 1, &error.to_string());
        return Err(error.into());
    }
    eprintln!(
        "mandala: build: done rc={rc} built={} fetched={} errors={}",
        tracker.built, tracker.fetched, tracker.errors
    );
    let final_snapshot = forest.snapshot();
    if let Some((path, cache)) = duration_cache.as_mut() {
        cache.observe_snapshot(&final_snapshot);
        if let Err(error) = cache.save(path) {
            eprintln!("mandala: build duration cache not saved: {error}");
        }
    }
    eprintln!(
        "mandala: {}",
        nix_build_forest::plain::render_final(&final_snapshot)
    );

    for node in final_snapshot
        .nodes
        .iter()
        .filter(|node| node.status == nix_build_forest::DerivationStatus::Failed)
    {
        let saved_log = match save_derivation_log(program, run, &node.path) {
            Ok(path) => format!("Saved log: {}", path.display()),
            Err(error) => format!("Full log unavailable locally: {error}"),
        };
        let message = format!(
            "failed derivation: {}\n{}\n{saved_log}\nFull log: nix log {}",
            node.path,
            node.log_tail.join("\n"),
            node.path
        );
        eprintln!("mandala: {message}");
        writer.emit(
            "line",
            event_fields([
                ("line", Value::from(message)),
                ("stream", Value::from("nix")),
            ]),
        )?;
    }

    match outcome {
        Ok(built) => {
            let mut fields = Meta::new();
            fields.insert("build_rc".into(), Value::from(process_rc));
            fields.insert(
                "build_failures".into(),
                serde_json::to_value(&built.failures).map_err(io::Error::other)?,
            );
            fields.insert(
                "profiles".into(),
                serde_json::to_value(&built.paths).map_err(io::Error::other)?,
            );
            registry::update_meta(&run.path, fields)?;
            Ok(built)
        }
        Err(error) => {
            record_build_failure(run, rc, &error.to_string());
            Err(error)
        }
    }
}

fn emit_milestone(writer: &EventWriter, milestone: &str) -> io::Result<()> {
    writer.emit(
        "milestone",
        event_fields([("milestone", Value::from(milestone))]),
    )
}

fn host_result(
    writer: &EventWriter,
    host: &str,
    state: HostState,
    error: Option<String>,
) -> HostDeployResult {
    let rc = i32::from(error.is_some());
    let event_error = writer
        .emit(
            "status",
            event_fields([("rc", Value::from(rc)), ("state", Value::from("done"))]),
        )
        .err()
        .map(|event_error| format!("writing terminal host status: {event_error}"));
    HostDeployResult {
        host: host.to_string(),
        state: if event_error.is_some() {
            HostState::Failed
        } else {
            state
        },
        error: event_error.or(error),
    }
}

fn failed_host(writer: &EventWriter, host: &str, error: impl Into<String>) -> HostDeployResult {
    let error = error.into();
    let _ = writer.emit(
        "line",
        event_fields([
            ("line", Value::from(error.clone())),
            ("stream", Value::from("deploy")),
        ]),
    );
    host_result(writer, host, HostState::Failed, Some(error))
}

/// Run one host's copy + activation pipeline from its flattened aggregate
/// settings and prebuilt profile. There is deliberately no fan-out here:
/// task 3.4 will call this async seam from its bounded JoinSet.
pub async fn deploy_host(
    run: &RegisteredDeployRun,
    built: &BuiltProfiles,
    host: &str,
) -> HostDeployResult {
    deploy_host_with(run, built, host, &DeployPrograms::default()).await
}

async fn deploy_host_with(
    run: &RegisteredDeployRun,
    built: &BuiltProfiles,
    host: &str,
    programs: &DeployPrograms,
) -> HostDeployResult {
    let writer = match EventWriter::new(&run.path, host, host, "deploy") {
        Ok(writer) => Arc::new(writer),
        Err(error) => {
            return HostDeployResult {
                host: host.to_string(),
                state: HostState::Failed,
                error: Some(format!("opening host event stream: {error}")),
            };
        }
    };
    if let Err(error) = writer.emit(
        "status",
        event_fields([
            (
                "cmd",
                Value::from(vec!["mandala-deploy".to_string(), host.to_string()]),
            ),
            ("state", Value::from("start")),
        ]),
    ) {
        return HostDeployResult {
            host: host.to_string(),
            state: HostState::Failed,
            error: Some(format!("writing host start status: {error}")),
        };
    }

    let Some(settings) = run.plan.settings.get(host) else {
        return failed_host(&writer, host, "flattened deploy settings are missing");
    };
    let settings = match serde_json::from_value::<FlattenedDeploySettings>(settings.clone()) {
        Ok(settings) => settings,
        Err(error) => {
            return failed_host(
                &writer,
                host,
                format!("invalid flattened deploy settings: {error}"),
            );
        }
    };
    let Some(profile_path) = built.paths.get(host) else {
        return failed_host(&writer, host, "prebuilt profile path is missing");
    };

    let profile = Profile {
        profile_settings: ProfileSettings {
            path: profile_path.display().to_string(),
            profile_path: None,
        },
        generic_settings: GenericSettings::default(),
    };
    let node = Node {
        generic_settings: settings.generic(),
        node_settings: NodeSettings {
            hostname: settings.hostname.clone(),
            profiles: HashMap::from([("system".to_string(), profile.clone())]),
            profiles_order: Vec::new(),
        },
    };
    let overrides = CmdOverrides {
        hostname: None,
        nix_program: programs.nix.clone(),
        ssh_program: programs.ssh.clone(),
        environment: programs.environment.clone(),
    };
    let sink = RegistryDeploySink::new(Arc::clone(&writer));
    let deploy = make_deploy_data(
        &GenericSettings::default(),
        &node,
        host,
        &profile,
        "system",
        &overrides,
        &sink,
    );
    let defs = match deploy.defs() {
        Ok(defs) => defs,
        Err(error) => return failed_host(&writer, host, error.to_string()),
    };

    if let Err(error) = emit_milestone(&writer, "copy") {
        return failed_host(&writer, host, format!("writing copy milestone: {error}"));
    }
    if let Err(error) = push_profile(&deploy, &defs, false).await {
        return failed_host(&writer, host, error.to_string());
    }
    if let Some(error) = sink.error() {
        return failed_host(&writer, host, format!("writing copied output: {error}"));
    }

    let boot = run.plan.boot || settings.activation == ActivationMode::Boot;
    let disposition =
        match resolve_activation_disposition(&deploy, &defs, run.plan.dry_activate, boot).await {
            Ok(disposition) => disposition,
            Err(error) => return failed_host(&writer, host, error.to_string()),
        };
    if let Some(error) = sink.error() {
        return failed_host(&writer, host, format!("writing preflight output: {error}"));
    }

    if disposition.reboot_pending() && run.plan.dry_activate {
        let _ = writer.emit(
            "line",
            event_fields([
                (
                    "line",
                    Value::from("dry activation: a real deployment would be staged for reboot"),
                ),
                ("stream", Value::from("deploy")),
            ]),
        );
        if let Err(error) = emit_milestone(&writer, "reboot-pending") {
            return failed_host(
                &writer,
                host,
                format!("writing reboot-pending milestone: {error}"),
            );
        }
        return host_result(&writer, host, HostState::RebootPending, None);
    }

    if let Err(error) = emit_milestone(&writer, "activate") {
        return failed_host(
            &writer,
            host,
            format!("writing activation milestone: {error}"),
        );
    }
    if matches!(disposition, ActivationDisposition::LiveSwitch)
        && settings.magic_rollback.unwrap_or(true)
        && let Err(error) = emit_milestone(&writer, "wait")
    {
        return failed_host(
            &writer,
            host,
            format!("writing activation-wait milestone: {error}"),
        );
    }
    let deployment = match disposition {
        ActivationDisposition::BootStaging => stage_profile_for_boot(&deploy, &defs).await,
        ActivationDisposition::LiveSwitch
        | ActivationDisposition::DryPreview {
            reboot_pending: false,
        } => deploy_profile(&deploy, &defs, run.plan.dry_activate).await,
        ActivationDisposition::DryPreview {
            reboot_pending: true,
        } => unreachable!("reboot-pending dry previews return before activation"),
    };
    match deployment {
        Ok(()) => {
            if let Some(error) = sink.error() {
                return failed_host(&writer, host, format!("writing activation output: {error}"));
            }
            let (milestone, state) = if matches!(disposition, ActivationDisposition::BootStaging) {
                ("reboot-pending", HostState::RebootPending)
            } else {
                ("confirm", HostState::Confirmed)
            };
            if let Err(error) = emit_milestone(&writer, milestone) {
                return failed_host(
                    &writer,
                    host,
                    format!("writing {milestone} milestone: {error}"),
                );
            }
            host_result(&writer, host, state, None)
        }
        Err(error) if error.rolled_back() => {
            let message = error.to_string();
            let _ = writer.emit(
                "line",
                event_fields([
                    ("line", Value::from(message.clone())),
                    ("stream", Value::from("deploy")),
                ]),
            );
            if let Err(event_error) = emit_milestone(&writer, "rollback") {
                return failed_host(
                    &writer,
                    host,
                    format!("writing rollback milestone: {event_error}"),
                );
            }
            host_result(&writer, host, HostState::RolledBack, Some(message))
        }
        Err(error) => failed_host(&writer, host, error.to_string()),
    }
}

fn abnormal_host(
    run: &RegisteredDeployRun,
    host: &str,
    error: impl Into<String>,
) -> HostDeployResult {
    let error = error.into();
    let writer = match EventWriter::new(&run.path, host, host, "deploy") {
        Ok(writer) => writer,
        Err(event_error) => {
            return HostDeployResult {
                host: host.to_string(),
                state: HostState::Failed,
                error: Some(format!("{error}; opening host event stream: {event_error}")),
            };
        }
    };
    let _ = writer.emit(
        "status",
        event_fields([
            (
                "cmd",
                Value::from(vec!["mandala-deploy".to_string(), host.to_string()]),
            ),
            ("state", Value::from("start")),
        ]),
    );
    failed_host(&writer, host, error)
}

fn summarize(results: &BTreeMap<String, HostDeployResult>) -> DeploySummary {
    DeploySummary {
        total: results.len(),
        confirmed: results
            .values()
            .filter(|result| result.state == HostState::Confirmed)
            .count(),
        reboot_pending: results
            .values()
            .filter(|result| result.state == HostState::RebootPending)
            .count(),
        failed: results
            .values()
            .filter(|result| result.state == HostState::Failed)
            .count(),
        rolled_back: results
            .values()
            .filter(|result| result.state == HostState::RolledBack)
            .count(),
    }
}

fn settle_fan_out(
    run: &RegisteredDeployRun,
    results: BTreeMap<String, HostDeployResult>,
) -> io::Result<DeployOutcome> {
    let summary = summarize(&results);
    // Every host-level error is contained and represented durably. Reaching
    // settlement means the native controller itself completed successfully;
    // sticky host outcomes synthesize the effective non-zero rc.
    let process_rc = 0;
    let build_failed = registry::read_meta(&run.path)
        .get("build_rc")
        .and_then(Value::as_i64)
        .is_some_and(|rc| rc != 0);
    let rc = i32::from(build_failed || summary.failed > 0 || summary.rolled_back > 0);
    let mut fields = Meta::new();
    fields.insert("process_rc".into(), Value::from(process_rc));
    fields.insert("rc".into(), Value::from(rc));
    fields.insert("finished_at".into(), Value::from(now_epoch_f64()));
    fields.insert(
        "summary".into(),
        serde_json::to_value(&summary).map_err(io::Error::other)?,
    );
    registry::update_meta(&run.path, fields)?;
    Ok(DeployOutcome {
        results,
        summary,
        process_rc,
        rc,
    })
}

fn record_controller_failure(run: &RegisteredDeployRun, error: &str) {
    let mut fields = Meta::new();
    fields.insert("process_rc".into(), Value::from(1));
    fields.insert("rc".into(), Value::from(1));
    fields.insert("error".into(), Value::from(error));
    fields.insert("finished_at".into(), Value::from(now_epoch_f64()));
    let _ = registry::update_meta(&run.path, fields);
}

fn skipped_notice(host: &str) -> String {
    format!("mandala: skipping {host}: deploy-rs is disabled")
}

fn outcome_lines(outcome: &DeployOutcome) -> Vec<String> {
    let mut lines = vec![format!(
        "mandala: deploy summary: total={} confirmed={} reboot-pending={} failed={} rolled-back={}",
        outcome.summary.total,
        outcome.summary.confirmed,
        outcome.summary.reboot_pending,
        outcome.summary.failed,
        outcome.summary.rolled_back
    )];
    for result in outcome
        .results
        .values()
        .filter(|result| matches!(result.state, HostState::Failed | HostState::RolledBack))
    {
        lines.push(format!(
            "mandala: deploy: {}: {}: {}",
            result.host,
            result.state,
            result.error.as_deref().unwrap_or("no diagnostic")
        ));
    }
    if outcome.rc == 0 && outcome.summary.reboot_pending == 0 {
        lines.push("mandala: deploy: all hosts confirmed".to_string());
    } else if outcome.rc == 0 {
        lines.push(format!(
            "mandala: deploy: {} host(s) staged successfully and require reboot",
            outcome.summary.reboot_pending
        ));
    } else {
        lines.push(
            "mandala: deploy: PARTIAL FAILURE — healthy siblings were not revoked".to_string(),
        );
    }
    lines
}

fn print_outcome(outcome: &DeployOutcome) {
    for line in outcome_lines(outcome) {
        eprintln!("{line}");
    }
}

/// Fan out one task per selected host, bounded by the validated throttle.
/// Panics and other JoinSet failures are mapped back to only their host and
/// every sibling is still joined before the run settles.
pub async fn deploy_hosts(
    run: &RegisteredDeployRun,
    built: &BuiltProfiles,
) -> io::Result<DeployOutcome> {
    let task: HostTask = Arc::new(|run, built, host| {
        Box::pin(async move { deploy_host(&run, &built, &host).await })
    });
    fan_out_with(run, built, task).await
}

async fn fan_out_with(
    run: &RegisteredDeployRun,
    built: &BuiltProfiles,
    task: HostTask,
) -> io::Result<DeployOutcome> {
    let throttle = usize::try_from(run.plan.throttle)
        .map_err(|_| io::Error::other("validated deploy throttle is outside usize"))?;
    if throttle == 0 {
        return Err(io::Error::other("validated deploy throttle is zero"));
    }

    let run = Arc::new(run.clone());
    let built = Arc::new(built.clone());
    let semaphore = Arc::new(Semaphore::new(throttle));
    let mut tasks = JoinSet::new();
    let mut task_hosts = HashMap::new();
    let mut results = BTreeMap::new();
    for host in &run.plan.targets {
        if let Some(error) = built.failures.get(host) {
            results.insert(host.clone(), abnormal_host(&run, host, error));
            continue;
        }
        let host = host.clone();
        let task_host = host.clone();
        let run = Arc::clone(&run);
        let built = Arc::clone(&built);
        let semaphore = Arc::clone(&semaphore);
        let task = Arc::clone(&task);
        let handle = tasks.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("the fan-out semaphore is never closed");
            task(run, built, task_host).await
        });
        task_hosts.insert(handle.id(), host);
    }

    while let Some(joined) = tasks.join_next_with_id().await {
        match joined {
            Ok((id, result)) => {
                task_hosts.remove(&id);
                results.insert(result.host.clone(), result);
            }
            Err(error) => {
                let host = task_hosts
                    .remove(&error.id())
                    .unwrap_or_else(|| "unknown-host".to_string());
                let result =
                    abnormal_host(&run, &host, format!("host task failed abnormally: {error}"));
                results.insert(host, result);
            }
        }
    }
    settle_fan_out(&run, results)
}

/// The argv for `deploy batch` — the group's eval-once artifact build. Pure so
/// tests assert the command line without spawning nix. Parity with the Python
/// `engines/deploy.py::batch` (group validated against `all` or a known group).
///
/// # Errors
/// [`InventoryError::NoSuchGroup`] if `group` is neither `all` nor a known
/// group (the Python `InventoryError(f"no such group: {group}")`).
pub fn batch_argv(
    inv: &Inventory,
    flake: &str,
    group: &str,
) -> Result<Vec<String>, InventoryError> {
    if group != "all" && !inv.groups().contains_key(group) {
        return Err(InventoryError::NoSuchGroup {
            group: group.to_string(),
            available: inv.groups().keys().cloned().collect(),
        });
    }
    Ok(vec![
        "nix".to_string(),
        "build".to_string(),
        "--no-link".to_string(),
        "--print-out-paths".to_string(),
        format!("{flake}#deployBatch.{group}"),
    ])
}

fn publish_run_id(mut writer: impl io::Write, run_id: &str) -> io::Result<()> {
    writeln!(writer, "{run_id}")?;
    writer.flush()
}

/// Dispatch the `deploy` engine's subcommand.
fn run(inv: &Inventory, m: &ArgMatches) -> ExitCode {
    // `--flake` is a global root arg, reachable through the engine's matches.
    let flake = m.get_one::<String>("flake").map_or(".", String::as_str);
    match m.subcommand() {
        Some(("run", sm)) => {
            let limit = sm.get_one::<String>("limit").map_or("", String::as_str);
            let throttle = *sm.get_one::<i64>("throttle").unwrap_or(&4);
            let dry_activate = sm.get_flag("dry-activate");
            let boot = sm.get_flag("boot");
            let prepared = plan_run_with_boot(inv, limit, throttle, dry_activate, boot).and_then(
                |mut plan| {
                    plan.halt_on_build_failure = sm.get_flag("halt-on-build-failure");
                    register_run(plan)
                },
            );
            match prepared {
                Ok(run) => {
                    if let Err(error) = publish_run_id(std::io::stdout().lock(), &run.run_id) {
                        let message = format!("publishing deploy run id: {error}");
                        record_controller_failure(&run, &message);
                        eprintln!("mandala: {message}");
                        return ExitCode::FAILURE;
                    }
                    for host in &run.plan.skipped {
                        eprintln!("{}", skipped_notice(host));
                    }
                    match build_profiles(&run, flake) {
                        Ok(built) => {
                            for (host, path) in &built.paths {
                                eprintln!("mandala: built {host}: {}", path.display());
                            }
                            let runtime = match tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()
                            {
                                Ok(runtime) => runtime,
                                Err(error) => {
                                    let message = format!("creating deploy runtime: {error}");
                                    record_controller_failure(&run, &message);
                                    eprintln!("mandala: {message}");
                                    return ExitCode::FAILURE;
                                }
                            };
                            match runtime.block_on(deploy_hosts(&run, &built)) {
                                Ok(outcome) => {
                                    print_outcome(&outcome);
                                    ExitCode::from(exit_byte(Some(outcome.rc)))
                                }
                                Err(error) => {
                                    let message = format!("settling deploy fan-out: {error}");
                                    record_controller_failure(&run, &message);
                                    eprintln!("mandala: {message}");
                                    ExitCode::FAILURE
                                }
                            }
                        }
                        Err(error) => {
                            eprintln!("mandala: {error}");
                            ExitCode::from(exit_byte(Some(error.exit_code())))
                        }
                    }
                }
                Err(err) => {
                    eprintln!("mandala: {err}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(("batch", sm)) => {
            let group = sm.get_one::<String>("group").map_or("", String::as_str);
            let argv = match batch_argv(inv, flake, group) {
                Ok(a) => a,
                Err(err) => {
                    eprintln!("mandala: {err}");
                    return ExitCode::FAILURE;
                }
            };
            let mut cmd = ProcCommand::new(&argv[0]);
            cmd.args(&argv[1..]);
            spawn_status(cmd)
        }
        Some(("nodes", _)) => {
            let mut nodes = inv.deploy_nodes();
            nodes.sort();
            for name in nodes {
                println!("{name}");
            }
            ExitCode::SUCCESS
        }
        // `subcommand_required` guarantees a matched arm.
        _ => ExitCode::from(2),
    }
}

/// Spawn a child, wait, and yield its exit code (Python `raise typer.Exit(
/// subprocess.run(...).returncode)`). A spawn failure is a hard error.
fn spawn_status(mut cmd: ProcCommand) -> ExitCode {
    match cmd.status() {
        Ok(status) => ExitCode::from(exit_byte(status.code())),
        Err(err) => {
            eprintln!("mandala: failed to run {:?}: {err}", cmd.get_program());
            ExitCode::FAILURE
        }
    }
}

/// Clamp a child's exit code into the `u8` an [`ExitCode`] carries (a
/// signal-killed child reports `None` → `1`).
fn exit_byte(code: Option<i32>) -> u8 {
    match code {
        Some(0) => 0,
        Some(c) => u8::try_from(c & 0xff).unwrap_or(1).max(1),
        None => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    /// `std::fs::write` for executable fixtures, WITHOUT this process ever
    /// holding a write fd to the file. Tests run on parallel threads, and every
    /// fork (each fake `nix`/`ssh` spawn) inherits the whole fd table until its
    /// exec; exec'ing a file some process still holds open for writing fails
    /// with ETXTBSY ("Text file busy"), which surfaced as random `Io` build and
    /// copy failures in the sandboxed nix check phase. A short-lived `sh` child
    /// does the write, so the fd never enters this process's table at all.
    fn write_program(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
        use std::io::Write as _;
        let mut child = std::process::Command::new("sh")
            .args(["-c", r#"cat > "$1""#, "sh"])
            .arg(path)
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(contents.as_ref())?;
        if child.wait()?.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "writing fixture {}",
                path.display()
            )))
        }
    }

    fn tmp_base(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "mandala-native-deploy-{label}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    fn inv() -> Inventory {
        Inventory::from_value(json!({
            "schemaVersion": 1,
            "members": {
                "web": {
                    "name": "web",
                    "deployment": {"deployRs": {"enable": true}}
                },
                "cache": {
                    "name": "cache",
                    "deployment": {"deployRs": {"enable": true}}
                },
                "router": {
                    "name": "router",
                    "deployment": {"deployRs": {"enable": false}}
                },
            },
            "groups": {
                "fleet": ["cache", "router", "web"],
                "k3s": ["cache", "web"],
                "gateway": ["router"]
            },
            "projections": {"deploy": {
                "nodes": ["web", "cache"],
                "settings": {
                    "cache": {"sshUser": "cache-admin", "confirmTimeout": 30},
                    "web": {"sshUser": "web-admin", "confirmTimeout": 60}
                }
            }},
        }))
        .unwrap()
    }

    fn build_stub(base: &Path, rc: i32) -> (PathBuf, PathBuf, PathBuf) {
        let script = base.join(format!("nix-stub-{rc}"));
        let invocations = base.join(format!("invocations-{rc}"));
        let args = base.join(format!("args-{rc}"));
        std::fs::create_dir_all(base).unwrap();
        write_program(
            &script,
            format!(
                r#"#!/bin/sh
if [ "$1" = log ]; then
  printf '%s\n' 'fixture derivation failure log'
  exit 0
fi
case " $* " in
  *' --dry-run '*)
    printf '['
    sep=
    for arg in "$@"; do
      case "$arg" in
        *#deploy.nodes.*.profiles.system.path)
          host=${{arg#*#deploy.nodes.}}
          host=${{host%.profiles.system.path}}
          printf '%s{{"drvPath":"/nix/store/00000000000000000000000000000000-%s-profile.drv","outputs":{{"out":"/nix/store/00000000000000000000000000000000-%s-profile"}}}}' "$sep" "$host" "$host"
          sep=,
          ;;
      esac
    done
    printf ']\n'
    exit 0
    ;;
  *' --offline '*) exit 1 ;;
esac
printf 'x\n' >> '{}'
printf '%s\n' "$@" > '{}'
printf '%s\n' '@nix {{"action":"start","id":7,"type":105,"fields":["/nix/store/0123456789abcdfghijklmnpqrsvwxyz-profile.drv"]}}' >&2
printf '%s\n' 'warning: stub diagnostic' >&2
printf '%s\n' '@nix {{"action":"stop","id":7}}' >&2
if [ {} -ne 0 ]; then
  printf '%s\n' '@nix {{"action":"msg","level":0,"raw_msg":"Cannot build /nix/store/0123456789abcdfghijklmnpqrsvwxyz-profile.drv because the builder failed"}}' >&2
  exit {}
fi
out_link=
want_out_link=0
hosts=
for arg in "$@"; do
  if [ "$want_out_link" -eq 1 ]; then
    out_link=$arg
    want_out_link=0
  elif [ "$arg" = '--out-link' ]; then
    want_out_link=1
  else
    case "$arg" in
      /nix/store/*-profile.drv^out)
        host=${{arg#/nix/store/00000000000000000000000000000000-}}
        host=${{host%-profile.drv^out}}
        hosts="$hosts $host"
        ;;
    esac
  fi
done
index=0
for host in $hosts; do
  link=$out_link
  if [ "$index" -ne 0 ]; then
    link="$out_link-$index"
  fi
  ln -s "/nix/store/00000000000000000000000000000000-$host-profile" "$link"
  index=$((index + 1))
done
"#,
                invocations.display(),
                args.display(),
                rc,
                rc,
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();
        (script, invocations, args)
    }

    fn effect_programs_with_cleanup(
        base: &Path,
        copy_rc: i32,
        cleanup_rc: i32,
        confirm_rc: i32,
    ) -> (DeployPrograms, PathBuf) {
        let trace = base.join(format!("effects-{copy_rc}-{confirm_rc}"));
        let nix = base.join(format!("nix-copy-{copy_rc}"));
        let ssh = base.join(format!("ssh-activate-{confirm_rc}"));
        std::fs::create_dir_all(base).unwrap();
        write_program(
            &nix,
            format!(
                r#"#!/bin/sh
printf 'nix USER=%s HOME=%s SSH_AUTH_SOCK=%s NIX_SSHOPTS=%s args=%s\n' "$USER" "$HOME" "$SSH_AUTH_SOCK" "$NIX_SSHOPTS" "$*" >> '{}'
printf '%s\n' 'nix-copy-stdout'
printf '%s\n' 'nix-copy-stderr' >&2
exit {}
"#,
                trace.display(),
                copy_rc,
            ),
        )
        .unwrap();
        write_program(
            &ssh,
            format!(
                r#"#!/bin/sh
printf 'ssh USER=%s HOME=%s SSH_AUTH_SOCK=%s args=%s\n' "$USER" "$HOME" "$SSH_AUTH_SOCK" "$*" >> '{trace}'
case "$*" in
  *'readlink -e /nix/var/nix/profiles/'*)
    printf '%s\n' '/nix/store/99999999999999999999999999999999-previous-system'
    exit 0
    ;;
esac
printf '%s\n' 'ssh-stdout'
printf '%s\n' 'ssh-stderr' >&2
wait_for_trace() {{
  attempt=0
  until grep -qs -- "$1" '{trace}'; do
    attempt=$((attempt + 1))
    if [ "$attempt" -gt 500 ]; then
      exit 99
    fi
    sleep 0.01
  done
}}
case "$*" in
  *' rm -f '*) exit {cleanup_rc} ;;
  *' rm '*)
    # The engine stops awaiting the waiter once confirmation settles (and
    # the activation once the waiter wins), so this exit is the last point
    # at which the fixture can guarantee both invocations already reached
    # the trace the tests read after deploy_host_with returns. The children
    # were spawned before confirmation started; only their first printf may
    # still be unscheduled under load.
    wait_for_trace 'activate-rs activate '
    wait_for_trace 'activate-rs wait '
    exit {confirm_rc}
    ;;
esac
exit 0
"#,
                trace = trace.display(),
            ),
        )
        .unwrap();
        for program in [&nix, &ssh] {
            let mut permissions = std::fs::metadata(program).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(program, permissions).unwrap();
        }
        (
            DeployPrograms {
                nix: Some(nix),
                ssh: Some(ssh),
                environment: vec![
                    ("USER".into(), "ambient-user-must-not-win".into()),
                    ("HOME".into(), "/ambient/home/must-not-win".into()),
                    ("SSH_AUTH_SOCK".into(), "/ambient/agent/must-not-win".into()),
                ],
            },
            trace,
        )
    }

    fn effect_programs(base: &Path, copy_rc: i32, confirm_rc: i32) -> (DeployPrograms, PathBuf) {
        effect_programs_with_cleanup(base, copy_rc, 0, confirm_rc)
    }

    fn inhibitor_programs(
        base: &Path,
        capability_rc: i32,
        check_rc: i32,
        boot_rc: i32,
    ) -> (DeployPrograms, PathBuf) {
        let (programs, trace) = effect_programs(base, 0, 0);
        let ssh = programs.ssh.as_ref().unwrap();
        write_program(
            ssh,
            format!(
                r#"#!/bin/sh
printf 'ssh args=%s\n' "$*" >> '{trace}'
case "$*" in
  *switch-inhibitors*) exit {capability_rc} ;;
  *'switch-to-configuration check'*) echo 'inhibitor-refused' >&2; exit {check_rc} ;;
  *'readlink -e '*) echo /nix/store/99999999999999999999999999999999-previous-system; exit 0 ;;
  *'nix-env --profile '*'-new-system'*) exit 0 ;;
  *'nix-env --profile '*'-profile'*) exit 0 ;;
  *'-profile/bin/switch-to-configuration boot'*) exit {boot_rc} ;;
  *'-previous-system/bin/switch-to-configuration boot'*) exit 0 ;;
  *' rm -f '*) exit 0 ;;
  *'activate-rs wait '*) exit 0 ;;
  *' rm '*) exit 0 ;;
  *'activate-rs activate '*) exit 0 ;;
esac
exit 99
"#,
                trace = trace.display(),
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(ssh).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(ssh, permissions).unwrap();
        (programs, trace)
    }

    #[derive(Clone, Copy)]
    enum ActivationEffectOrder {
        ConfirmationFirst,
        ConfirmationAfterActivation,
        ActivationBeforeWait,
    }

    impl ActivationEffectOrder {
        fn shell_name(self) -> &'static str {
            match self {
                Self::ConfirmationFirst => "confirmation-first",
                Self::ConfirmationAfterActivation => "confirmation-after-activation",
                Self::ActivationBeforeWait => "activation-before-wait",
            }
        }
    }

    fn ordered_activation_programs(
        base: &Path,
        confirm_rc: i32,
        activate_rc: i32,
        order: ActivationEffectOrder,
    ) -> (DeployPrograms, PathBuf) {
        let order_name = order.shell_name();
        let trace = base.join(format!(
            "ordered-effects-{order_name}-{confirm_rc}-{activate_rc}"
        ));
        let confirmed = base.join(format!(
            "confirmation-attempted-{order_name}-{confirm_rc}-{activate_rc}"
        ));
        let nix = base.join(format!(
            "ordered-nix-copy-{order_name}-{confirm_rc}-{activate_rc}"
        ));
        let ssh = base.join(format!(
            "ordered-ssh-activate-{order_name}-{confirm_rc}-{activate_rc}"
        ));
        std::fs::create_dir_all(base).unwrap();
        write_program(
            &nix,
            format!(
                r#"#!/bin/sh
printf 'nix args=%s\n' "$*" >> '{}'
exit 0
"#,
                trace.display(),
            ),
        )
        .unwrap();
        // Every ordering constraint below is "block until the ENGINE has
        // recorded X in the host's event stream" (`<runs base>/<run>/cache.jsonl`,
        // which the sink appends every child line and log message to), never
        // "a sibling script wrote a marker, so sleep a bit". A sibling exiting
        // says nothing about when the tokio side reaps it: under a starved
        // sandbox both children become ready in one poll and an unbiased
        // `select!` picks at random — the flake this replaces. Child stdout
        // reaches the same stream via `emit_output`, which is why the fake
        // confirmation echoes a sentinel: `checked_output` forwards it in the
        // same poll that resolves the confirmation branch, on success and on
        // failure alike.
        write_program(
            &ssh,
            format!(
                r#"#!/bin/sh
printf 'ssh args=%s\n' "$*" >> '{trace}'
order='{order_name}'
wait_for_log() {{
  attempt=0
  until grep -qs -- "$1" '{base}'/*/cache.jsonl; do
    attempt=$((attempt + 1))
    if [ "$attempt" -gt 500 ]; then
      exit 99
    fi
    sleep 0.01
  done
}}
wait_for_marker() {{
  attempt=0
  while [ ! -e "$1" ]; do
    attempt=$((attempt + 1))
    if [ "$attempt" -gt 500 ]; then
      exit 99
    fi
    sleep 0.01
  done
}}
case "$*" in
  *' rm -f '*) exit 0 ;;
  *'activate-rs activate '*)
    case "$order" in
      confirmation-first)
        # The engine has judged the confirmation (the biased select already
        # committed to that branch while activation was still pending).
        wait_for_log 'confirmation exited'
        ;;
      confirmation-after-activation)
        # The waiter branch won and the confirmation child has actually
        # started (its trace line is already on disk), so this exit lands
        # in the second select as a post-wait rollback, not a failure.
        wait_for_log 'activation waiter complete'
        wait_for_marker '{confirmed}'
        ;;
      activation-before-wait)
        ;;
    esac
    exit {activate_rc}
    ;;
  *'activate-rs wait '*)
    case "$order" in
      activation-before-wait)
        # Stay pending until the engine has already classified the early
        # activation exit; only then may the waiter settle.
        wait_for_log 'ssh activation failed'
        ;;
    esac
    exit 0
    ;;
  *' rm '*)
    case "$order" in
      activation-before-wait)
        exit 98
        ;;
    esac
    : > '{confirmed}'
    case "$order" in
      confirmation-after-activation)
        # Never complete before the activation exit is judged; the engine
        # kills this child (kill_on_drop) once it returns the rollback.
        wait_for_log 'activation rolled back'
        ;;
    esac
    echo 'confirmation exited'
    exit {confirm_rc}
    ;;
esac
exit 0
"#,
                trace = trace.display(),
                base = base.display(),
                confirmed = confirmed.display(),
            ),
        )
        .unwrap();
        for program in [&nix, &ssh] {
            let mut permissions = std::fs::metadata(program).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(program, permissions).unwrap();
        }
        (
            DeployPrograms {
                nix: Some(nix),
                ssh: Some(ssh),
                environment: Vec::new(),
            },
            trace,
        )
    }

    fn deploy_settings(activation: &str) -> Value {
        json!({
            "activation": activation,
            "hostname": "cache.example.test",
            "sshUser": "deployer",
            "sshPort": 2222,
            "identityFile": "/keys/mandala-test",
            "sshOpts": ["-o", "StrictHostKeyChecking=no"],
            "autoRollback": true,
            "fastConnection": false,
            "magicRollback": true,
            "confirmTimeout": 41,
            "activationTimeout": 97,
            "tempPath": "/run/mandala",
            "sudo": "doas -u",
            "user": "app"
        })
    }

    fn minimal_parent_deploy_settings() -> Value {
        json!({
            "activation": "switch",
            "hostname": "172.16.42.7",
            "sshUser": "root",
            "sshPort": 22,
            "identityFile": "/Users/bryan/.ssh/id_rsa",
            "autoRollback": true,
            "fastConnection": true
        })
    }

    fn built_cache_profile() -> BuiltProfiles {
        BuiltProfiles {
            failures: BTreeMap::new(),
            paths: BTreeMap::from([(
                "cache".into(),
                PathBuf::from("/nix/store/00000000000000000000000000000000-cache-profile"),
            )]),
        }
    }

    fn configured_k3s_run(base: &Path, throttle: i64) -> RegisteredDeployRun {
        let mut run = prepare_run(&inv(), "@k3s", throttle, true).unwrap();
        for host in ["cache", "web"] {
            run.plan
                .settings
                .insert(host.to_string(), deploy_settings("switch"));
        }
        registry::update_meta(
            &run.path,
            Meta::from_iter([("build_rc".into(), Value::from(0))]),
        )
        .unwrap();
        assert!(run.path.starts_with(base));
        run
    }

    fn built_k3s_profiles() -> BuiltProfiles {
        BuiltProfiles {
            failures: BTreeMap::new(),
            paths: BTreeMap::from([
                (
                    "cache".into(),
                    PathBuf::from("/nix/store/00000000000000000000000000000000-cache-profile"),
                ),
                (
                    "web".into(),
                    PathBuf::from("/nix/store/11111111111111111111111111111111-web-profile"),
                ),
            ]),
        }
    }

    fn program_task(programs: BTreeMap<String, DeployPrograms>) -> HostTask {
        Arc::new(move |run, built, host| {
            let programs = programs[&host].clone();
            Box::pin(async move { deploy_host_with(&run, &built, &host, &programs).await })
        })
    }

    fn jsonl(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn plan_resolves_selection_skips_non_deploy_members_and_reads_settings() {
        let plan = plan_run(&inv(), "@fleet", 8, true).unwrap();
        assert_eq!(plan.limit, "cache,router,web");
        assert_eq!(plan.targets, ["cache", "web"]);
        assert_eq!(plan.skipped, ["router"]);
        assert_eq!(
            plan.skipped
                .iter()
                .map(|host| skipped_notice(host))
                .collect::<Vec<_>>(),
            ["mandala: skipping router: deploy-rs is disabled"]
        );
        assert_eq!(plan.throttle, 8);
        assert!(plan.dry_activate);
        assert!(!plan.boot);
        assert_eq!(
            plan.settings["cache"]["sshUser"],
            Value::from("cache-admin")
        );
        assert_eq!(plan.settings["web"]["confirmTimeout"], Value::from(60));

        let err = plan_run(&inv(), "ghost", 4, false).unwrap_err();
        assert!(matches!(err, DeployRunError::Inventory(_)));
    }

    #[test]
    fn zero_deployable_selection_refuses_before_registry_creation() {
        let base = tmp_base("empty");
        let _guard = registry::test_hooks::install_runs_base(base.clone());

        let err = prepare_run(&inv(), "@gateway", 4, false).unwrap_err();
        assert!(matches!(err, DeployRunError::NoDeployableMembers(_)));
        assert!(!base.exists());
    }

    #[test]
    fn run_id_publication_is_one_flushed_line() {
        #[derive(Default)]
        struct RecordingWriter {
            bytes: Vec<u8>,
            flushed: bool,
        }
        impl io::Write for RecordingWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                self.flushed = true;
                Ok(())
            }
        }
        let mut writer = RecordingWriter::default();
        publish_run_id(&mut writer, "20260721T120000_123456-42").unwrap();
        assert_eq!(writer.bytes, b"20260721T120000_123456-42\n");
        assert!(writer.flushed);
    }

    #[test]
    fn non_positive_throttle_refuses_before_registry_creation() {
        for throttle in [0, -3] {
            let base = tmp_base(&format!("throttle-{throttle}"));
            let _guard = registry::test_hooks::install_runs_base(base.clone());
            let error = prepare_run(&inv(), "cache", throttle, false).unwrap_err();
            assert!(matches!(error, DeployRunError::InvalidThrottle(value) if value == throttle));
            assert!(!base.exists());
        }
    }

    #[test]
    fn registry_run_has_metadata_and_one_stream_per_target() {
        let base = tmp_base("registry");
        let _guard = registry::test_hooks::install_runs_base(base);

        let run = prepare_run(&inv(), "@fleet", 8, true).unwrap();
        assert!(run.path.join("cache.jsonl").is_file());
        assert!(run.path.join("web.jsonl").is_file());
        assert!(!run.path.join("router.jsonl").exists());
        let meta = registry::read_meta(&run.path);
        assert_eq!(meta["run_id"], Value::from(run.run_id));
        assert_eq!(meta["kind"], Value::from("deploy"));
        assert_eq!(meta["limit"], Value::from("cache,router,web"));
        assert_eq!(meta["targets"], json!(["cache", "web"]));
        assert_eq!(meta["skipped"], json!(["router"]));
        assert_eq!(meta["dry_activate"], Value::from(true));
        assert_eq!(meta["boot"], Value::from(false));
        assert_eq!(meta["throttle"], Value::from(8));
        assert!(meta["started_at"].as_f64().is_some());
        assert_eq!(meta["pid"], Value::from(i64::from(std::process::id())));
    }

    #[test]
    fn build_argv_is_one_targeted_impure_invocation() {
        let argv = build_run_argv(
            "/fleet",
            &["cache".into(), "web".into()],
            Path::new("/run/profile"),
        );
        assert_eq!(
            argv,
            [
                "nix",
                "build",
                "/fleet#deploy.nodes.cache.profiles.system.path",
                "/fleet#deploy.nodes.web.profiles.system.path",
                "--keep-going",
                "--log-format",
                "internal-json",
                "--impure",
                "--out-link",
                "/run/profile",
            ]
        );
        assert!(!argv.iter().any(|arg| arg.contains("router")));
        assert!(!argv.iter().any(|arg| arg.contains("deployBatch")));
    }

    #[test]
    fn one_build_maps_profiles_and_tees_compatible_events() {
        let base = tmp_base("build-success");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let run = prepare_run(&inv(), "@fleet", 4, false).unwrap();
        let (stub, invocations, args) = build_stub(&base, 0);

        let built = build_profiles_with(&run, "/fleet", stub.as_os_str()).unwrap();
        assert_eq!(
            built.paths,
            BTreeMap::from([
                (
                    "cache".into(),
                    PathBuf::from("/nix/store/00000000000000000000000000000000-cache-profile",),
                ),
                (
                    "web".into(),
                    PathBuf::from("/nix/store/00000000000000000000000000000000-web-profile",),
                ),
            ])
        );
        assert_eq!(std::fs::read_to_string(invocations).unwrap(), "x\n");
        let args = std::fs::read_to_string(args).unwrap();
        assert!(
            args.contains("/nix/store/00000000000000000000000000000000-cache-profile.drv^out\n")
        );
        assert!(args.contains("/nix/store/00000000000000000000000000000000-web-profile.drv^out\n"));
        assert!(!args.contains("router"));

        let build_stream = std::fs::read_to_string(run.path.join("build.jsonl")).unwrap();
        let nixlog_bytes = build_stream
            .lines()
            .find(|line| line.contains("\"event\":\"nixlog\""))
            .unwrap();
        assert!(nixlog_bytes.starts_with("{\"v\":2,\"ts\":"));
        assert!(nixlog_bytes.contains(
            "\"host\":\"controller\",\"plugin\":\"build\",\"event\":\"nixlog\",\"line\":\"@nix "
        ));
        assert!(!nixlog_bytes.contains(": "));
        let events = jsonl(&run.path.join("build.jsonl"));
        assert_eq!(events.first().unwrap()["event"], "status");
        assert_eq!(events.first().unwrap()["state"], "start");
        assert!(events.iter().all(|event| event["v"] == 2));
        assert!(
            events
                .iter()
                .all(|event| event["host"] == "controller" && event["plugin"] == "build")
        );
        assert!(events.iter().any(|event| {
            event["event"] == "nixlog"
                && event["line"]
                    == "@nix {\"action\":\"start\",\"id\":7,\"type\":105,\"fields\":[\"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-profile.drv\"]}"
        }));
        assert!(events.iter().any(|event| {
            event["event"] == "line"
                && event["line"] == "warning: stub diagnostic"
                && event["stream"] == "nix"
        }));
        assert!(events.iter().any(|event| {
            event["event"] == "progress" && event["built"] == 1 && event["finished"] == 1
        }));
        assert!(events.iter().any(|event| {
            event["event"] == "status" && event["state"] == "done" && event["rc"] == 0
        }));

        let sink = Arc::new(Mutex::new(Vec::new()));
        let sink_copy = Arc::clone(&sink);
        let mut tailer = crate::runner::EventTailer::new(&run.path);
        tailer.nixlog_sink = Some(Box::new(move |line| sink_copy.lock().unwrap().push(line)));
        tailer.poll();
        assert!(tailer.build.done);
        assert_eq!(tailer.build.rc, Some(0));
        assert!(
            tailer
                .build
                .lines
                .contains(&"warning: stub diagnostic".to_string())
        );
        assert_eq!(sink.lock().unwrap().len(), 2);

        let meta = registry::read_meta(&run.path);
        assert_eq!(meta["build_rc"], 0);
        assert_eq!(
            meta["profiles"]["cache"],
            "/nix/store/00000000000000000000000000000000-cache-profile"
        );
        assert!(meta.get("rc").is_none());
    }

    /// A complete native run leaves the fleet-state-formats registry shape:
    /// one dedicated build stream, pre-created per-host streams, indexed
    /// profile out-links, and the sorted one-space metadata document. Dynamic
    /// identity/timestamps are type-checked while every stable key and value is
    /// pinned explicitly.
    #[tokio::test]
    async fn native_engine_registry_layout_is_golden() {
        let base = tmp_base("registry-layout");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = prepare_run(&inv(), "@k3s", 2, true).unwrap();
        for host in ["cache", "web"] {
            run.plan
                .settings
                .insert(host.to_string(), deploy_settings("switch"));
        }
        let (stub, _invocations, _args) = build_stub(&base, 0);
        let built = build_profiles_with(&run, "/fleet", stub.as_os_str()).unwrap();
        let (programs, _trace) = effect_programs(&base, 0, 0);
        let task = program_task(BTreeMap::from([
            ("cache".into(), programs.clone()),
            ("web".into(), programs),
        ]));
        let outcome = fan_out_with(&run, &built, task).await.unwrap();
        assert_eq!(outcome.rc, 0);

        let mut entries = std::fs::read_dir(&run.path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(
            entries,
            [
                "build.jsonl",
                "cache.jsonl",
                "meta.json",
                "profile",
                "profile-1",
                "web.jsonl",
            ]
        );
        assert_eq!(
            std::fs::read_link(run.path.join("profile")).unwrap(),
            PathBuf::from("/nix/store/00000000000000000000000000000000-cache-profile")
        );
        assert_eq!(
            std::fs::read_link(run.path.join("profile-1")).unwrap(),
            PathBuf::from("/nix/store/00000000000000000000000000000000-web-profile")
        );

        let meta = registry::read_meta(&run.path);
        assert_eq!(
            meta.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "boot",
                "build_failures",
                "build_rc",
                "dry_activate",
                "finished_at",
                "halt_on_build_failure",
                "kind",
                "limit",
                "pid",
                "process_rc",
                "profiles",
                "rc",
                "run_id",
                "skipped",
                "started_at",
                "summary",
                "targets",
                "throttle",
            ]
        );
        assert_eq!(meta["kind"], "deploy");
        assert_eq!(meta["limit"], "cache,web");
        assert_eq!(meta["targets"], json!(["cache", "web"]));
        assert_eq!(meta["skipped"], json!([]));
        assert_eq!(meta["dry_activate"], true);
        assert_eq!(meta["boot"], false);
        assert_eq!(meta["throttle"], 2);
        assert_eq!(meta["build_rc"], 0);
        assert_eq!(meta["process_rc"], 0);
        assert_eq!(meta["rc"], 0);
        assert_eq!(
            meta["summary"],
            json!({"confirmed":2,"failed":0,"reboot_pending":0,"rolled_back":0,"total":2})
        );
        assert_eq!(meta["run_id"], run.run_id);
        assert_eq!(meta["pid"], i64::from(std::process::id()));
        assert!(meta["started_at"].is_f64());
        assert!(meta["finished_at"].is_f64());

        for stream in ["build", "cache", "web"] {
            let bytes = std::fs::read_to_string(run.path.join(format!("{stream}.jsonl"))).unwrap();
            assert!(!bytes.is_empty());
            assert!(bytes.ends_with('\n'));
            assert!(
                !bytes.contains("\": "),
                "{stream} JSONL field separators must stay compact"
            );
            assert!(bytes.lines().all(|line| {
                let event: Value = serde_json::from_str(line).unwrap();
                event["v"] == crate::runner::EVENT_PROTOCOL_VERSION
            }));
        }
    }

    #[tokio::test]
    async fn partial_build_deploys_only_the_verified_sibling() {
        let base = tmp_base("build-partial");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let run = prepare_run(&inv(), "@k3s", 4, false).unwrap();
        let (stub, invocations, _) = build_stub(&base, 23);
        // Nix leaves no batch out-links on failure. Only web's exact output
        // is valid, even though cache appeared first in the selection.
        let script = std::fs::read_to_string(&stub).unwrap().replace(
            "*' --offline '*) exit 1 ;;",
            r#"*' --offline '*)
              case "$2" in
                *-web-profile)
                  path=$2
                  for arg in "$@"; do link=$arg; done
                  ln -s "$path" "$link"
                  exit 0 ;;
                *) exit 1 ;;
              esac ;;
            "#,
        );
        write_program(&stub, script).unwrap();
        let built = build_profiles_with(&run, "/fleet", stub.as_os_str()).unwrap();
        assert_eq!(built.paths.keys().collect::<Vec<_>>(), ["web"]);
        assert_eq!(built.failures.keys().collect::<Vec<_>>(), ["cache"]);
        assert_eq!(std::fs::read_to_string(invocations).unwrap(), "x\n");
        assert_eq!(
            std::fs::read_link(run.path.join("profile-1")).unwrap(),
            built.paths["web"]
        );
        let task: HostTask = Arc::new(|run, _, host| {
            assert_eq!(host, "web", "failed hosts must not start deployment");
            Box::pin(async move {
                let writer = EventWriter::new(&run.path, &host, &host, "deploy").unwrap();
                emit_milestone(&writer, "confirm").unwrap();
                host_result(&writer, &host, HostState::Confirmed, None)
            })
        });
        let outcome = fan_out_with(&run, &built, task).await.unwrap();
        assert_eq!(outcome.summary.confirmed, 1);
        assert_eq!(outcome.summary.failed, 1);
        assert_eq!(outcome.rc, 1);
        let mut tailer = crate::runner::EventTailer::new(&run.path);
        tailer.poll();
        assert_eq!(tailer.hosts["cache"].state, HostState::Failed);
        assert_eq!(tailer.hosts["web"].state, HostState::Confirmed);
        let stream = std::fs::read_to_string(run.path.join("build.jsonl")).unwrap();
        assert!(stream.contains("failed derivation:"));
        assert_eq!(
            std::fs::read_to_string(
                run.path
                    .join("build-logs/0123456789abcdfghijklmnpqrsvwxyz-profile.log")
            )
            .unwrap(),
            "fixture derivation failure log\n"
        );
        assert!(stream.contains("Full log: nix log /nix/store/"));
        let meta = registry::read_meta(&run.path);
        assert_eq!(meta["build_rc"], 23);
        assert_eq!(meta["process_rc"], 0);
        assert_eq!(meta["rc"], 1);
    }

    #[test]
    fn halt_on_build_failure_is_opt_in_and_prevents_recovery() {
        let base = tmp_base("build-halt");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut plan = plan_run(&inv(), "@k3s", 4, false).unwrap();
        assert!(!plan.halt_on_build_failure);
        plan.halt_on_build_failure = true;
        let run = register_run(plan).unwrap();
        let (stub, _, args) = build_stub(&base, 23);
        assert!(matches!(
            build_profiles_with(&run, "/fleet", stub.as_os_str()),
            Err(BuildError::Failed(23))
        ));
        assert!(
            !std::fs::read_to_string(args)
                .unwrap()
                .contains("--keep-going")
        );
        let meta = registry::read_meta(&run.path);
        assert_eq!(meta["halt_on_build_failure"], true);
        assert_eq!(meta["rc"], 23);
        assert!(meta.get("profiles").is_none());
    }

    #[test]
    fn interrupted_build_never_recovers_or_deploys() {
        let base = tmp_base("build-interrupted");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let run = prepare_run(&inv(), "@k3s", 4, false).unwrap();
        let (stub, _, _) = build_stub(&base, 130);
        assert!(matches!(
            build_profiles_with(&run, "/fleet", stub.as_os_str()),
            Err(BuildError::Failed(130))
        ));
        assert_eq!(registry::read_meta(&run.path)["rc"], 130);
    }

    #[tokio::test]
    async fn failed_build_marks_every_host_without_deploying() {
        let base = tmp_base("build-failure");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let run = prepare_run(&inv(), "@k3s", 4, false).unwrap();
        let (stub, invocations, _args) = build_stub(&base, 23);

        let built = build_profiles_with(&run, "/fleet", stub.as_os_str()).unwrap();
        assert!(built.paths.is_empty());
        assert_eq!(built.failures.len(), 2);
        let task: HostTask = Arc::new(|_, _, _| panic!("failed build must never deploy"));
        let outcome = fan_out_with(&run, &built, task).await.unwrap();
        assert_eq!(outcome.summary.failed, 2);
        assert_eq!(outcome.rc, 1);
        assert_eq!(std::fs::read_to_string(invocations).unwrap(), "x\n");

        let meta = registry::read_meta(&run.path);
        assert_eq!(meta["build_rc"], 23);
        assert_eq!(meta["rc"], 1);
        assert!(meta["finished_at"].as_f64().is_some());
        assert!(
            meta["build_failures"]["cache"]
                .as_str()
                .unwrap()
                .contains("rc=23")
        );
        assert_eq!(meta["profiles"], json!({}));

        let mut tailer = crate::runner::EventTailer::new(&run.path);
        tailer.poll();
        assert!(tailer.build.done);
        assert_eq!(tailer.build.rc, Some(23));
        let snapshot = tailer.forest.snapshot();
        assert_eq!(snapshot.counts.failed, 1);
        assert_eq!(snapshot.failed_derivations, ["profile"]);
        let summary = nix_build_forest::plain::render_final(&snapshot);
        assert!(summary.contains("1 failed"));
        assert!(summary.contains("failed: profile"));
        assert!(run.plan.targets.iter().all(|host| {
            !profile_link(
                &run.path.join("profile"),
                run.plan
                    .targets
                    .iter()
                    .position(|candidate| candidate == host)
                    .unwrap(),
            )
            .exists()
        }));
    }

    #[tokio::test]
    async fn one_host_uses_flattened_settings_prebuilt_path_and_boot_mode() {
        let base = tmp_base("host-boot");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = prepare_run(&inv(), "cache", 4, false).unwrap();
        run.plan
            .settings
            .insert("cache".into(), deploy_settings("boot"));
        let (programs, trace) = effect_programs(&base, 0, 0);

        let result = deploy_host_with(&run, &built_cache_profile(), "cache", &programs).await;
        assert_eq!(result.state, HostState::RebootPending);
        assert_eq!(result.error, None);

        let trace = std::fs::read_to_string(trace).unwrap();
        assert!(trace.contains(
            "nix USER=ambient-user-must-not-win HOME=/ambient/home/must-not-win SSH_AUTH_SOCK=/ambient/agent/must-not-win NIX_SSHOPTS=-p 2222 -i /keys/mandala-test -o IdentitiesOnly=yes -o IdentityAgent=none -o StrictHostKeyChecking=no args=copy --substitute-on-destination --no-check-sigs --to ssh://deployer@cache.example.test /nix/store/00000000000000000000000000000000-cache-profile"
        ));
        assert!(trace.contains("nix-env --profile /nix/var/nix/profiles/per-user/app/system --set /nix/store/00000000000000000000000000000000-cache-profile"));
        assert!(trace.contains("cache-profile/bin/switch-to-configuration boot"));
        assert!(!trace.contains("activate-rs activate"));
        assert!(!trace.contains(" activate-rs wait "));
        assert!(!trace.contains(" rm "));

        let events = jsonl(&run.path.join("cache.jsonl"));
        assert!(events.iter().all(|event| {
            event["v"] == 2 && event["host"] == "cache" && event["plugin"] == "deploy"
        }));
        for line in [
            "nix-copy-stdout",
            "nix-copy-stderr",
            "ssh-stdout",
            "ssh-stderr",
        ] {
            assert!(events.iter().any(|event| {
                event["event"] == "line" && event["stream"] == "deploy" && event["line"] == line
            }));
        }
        let mut tailer = crate::runner::EventTailer::new(&run.path);
        tailer.poll();
        assert_eq!(tailer.hosts["cache"].state, HostState::RebootPending);
        assert_eq!(tailer.hosts["cache"].rc, Some(0));
        assert_eq!(
            tailer.hosts["cache"].milestones,
            ["copy", "activate", "reboot-pending"]
        );
    }

    #[tokio::test]
    async fn run_boot_override_uses_boot_mode_for_switch_member() {
        let base = tmp_base("host-run-boot");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = prepare_run_with_boot(&inv(), "cache", 4, false, true).unwrap();
        run.plan
            .settings
            .insert("cache".into(), deploy_settings("switch"));
        let (programs, trace) = effect_programs(&base, 0, 0);

        let result = deploy_host_with(&run, &built_cache_profile(), "cache", &programs).await;
        assert_eq!(result.state, HostState::RebootPending);
        let trace = std::fs::read_to_string(trace).unwrap();
        assert!(trace.contains("nix-env --profile"));
        assert!(trace.contains("switch-to-configuration boot"));
        assert!(!trace.contains("activate-rs activate"));
        assert!(!trace.contains(" activate-rs wait "));
        assert!(!trace.contains(" rm "));
    }

    #[tokio::test]
    async fn inhibitor_refusal_stages_and_dry_preview_never_mutates() {
        let base = tmp_base("host-inhibitor");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = prepare_run(&inv(), "cache", 4, false).unwrap();
        run.plan
            .settings
            .insert("cache".into(), deploy_settings("switch"));
        let (programs, trace) = inhibitor_programs(&base, 0, 42, 0);

        let result = deploy_host_with(&run, &built_cache_profile(), "cache", &programs).await;
        assert_eq!(result.state, HostState::RebootPending);
        let trace = std::fs::read_to_string(trace).unwrap();
        assert!(trace.contains("switch-to-configuration check"));
        assert!(trace.contains("nix-env --profile"));
        assert!(trace.contains("switch-to-configuration boot"));
        assert!(!trace.contains("activate-rs activate"));
        let events = jsonl(&run.path.join("cache.jsonl"));
        assert!(
            events
                .iter()
                .any(|event| { event["event"] == "line" && event["line"] == "inhibitor-refused" })
        );

        let dry_base = tmp_base("host-inhibitor-dry");
        let _dry_guard = registry::test_hooks::install_runs_base(dry_base.clone());
        let mut dry = prepare_run(&inv(), "cache", 4, true).unwrap();
        dry.plan
            .settings
            .insert("cache".into(), deploy_settings("switch"));
        let (programs, trace) = inhibitor_programs(&dry_base, 0, 42, 0);
        let result = deploy_host_with(&dry, &built_cache_profile(), "cache", &programs).await;
        assert_eq!(result.state, HostState::RebootPending);
        let trace = std::fs::read_to_string(trace).unwrap();
        assert!(trace.contains("switch-to-configuration check"));
        assert!(!trace.contains("nix-env --profile"));
        assert!(!trace.contains("switch-to-configuration boot"));
        assert!(!trace.contains("activate-rs"));
    }

    #[tokio::test]
    async fn legacy_closure_without_marker_preserves_live_switch() {
        let base = tmp_base("host-legacy-switch");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = prepare_run(&inv(), "cache", 4, false).unwrap();
        run.plan
            .settings
            .insert("cache".into(), deploy_settings("switch"));
        let (programs, trace) = inhibitor_programs(&base, 1, 99, 99);
        let result = deploy_host_with(&run, &built_cache_profile(), "cache", &programs).await;
        assert_eq!(result.state, HostState::Confirmed);
        let trace = std::fs::read_to_string(trace).unwrap();
        assert!(!trace.contains("switch-to-configuration check"));
        assert!(trace.contains("activate-rs activate"));
        assert!(!trace.contains("switch-to-configuration boot"));
    }

    #[tokio::test]
    async fn mixed_confirmed_and_reboot_pending_is_successful_but_not_all_confirmed() {
        let base = tmp_base("fanout-mixed-disposition");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = configured_k3s_run(&base, 2);
        run.plan.dry_activate = false;
        let (cache, _) = inhibitor_programs(&base.join("cache-effects"), 0, 42, 0);
        let (web, _) = inhibitor_programs(&base.join("web-effects"), 0, 0, 0);
        let task = program_task(BTreeMap::from([
            ("cache".into(), cache),
            ("web".into(), web),
        ]));

        let outcome = fan_out_with(&run, &built_k3s_profiles(), task)
            .await
            .unwrap();
        assert_eq!(outcome.results["cache"].state, HostState::RebootPending);
        assert_eq!(outcome.results["web"].state, HostState::Confirmed);
        assert_eq!(outcome.summary.confirmed, 1);
        assert_eq!(outcome.summary.reboot_pending, 1);
        assert_eq!(outcome.rc, 0);
        let lines = outcome_lines(&outcome);
        assert!(lines.iter().any(|line| line.contains("require reboot")));
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("all hosts confirmed"))
        );
        let meta = registry::read_meta(&run.path);
        assert_eq!(meta["summary"]["confirmed"], 1);
        assert_eq!(meta["summary"]["reboot_pending"], 1);
    }

    #[tokio::test]
    async fn reboot_pending_with_rolled_back_sibling_remains_a_failed_run() {
        let base = tmp_base("fanout-staged-and-rollback");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = configured_k3s_run(&base, 2);
        run.plan.dry_activate = false;
        let (cache, _) = inhibitor_programs(&base.join("cache-effects"), 0, 42, 0);
        let (web, _) = effect_programs(&base.join("web-effects"), 0, 42);
        let task = program_task(BTreeMap::from([
            ("cache".into(), cache),
            ("web".into(), web),
        ]));

        let outcome = fan_out_with(&run, &built_k3s_profiles(), task)
            .await
            .unwrap();
        assert_eq!(outcome.results["cache"].state, HostState::RebootPending);
        assert_eq!(outcome.results["web"].state, HostState::RolledBack);
        assert_eq!(outcome.summary.reboot_pending, 1);
        assert_eq!(outcome.summary.rolled_back, 1);
        assert_eq!(outcome.rc, 1);
        let lines = outcome_lines(&outcome);
        assert!(lines.iter().any(|line| line.contains("PARTIAL FAILURE")));
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("all hosts confirmed"))
        );
        let meta = registry::read_meta(&run.path);
        assert_eq!(meta["rc"], 1);
        assert_eq!(meta["summary"]["reboot_pending"], 1);
        assert_eq!(meta["summary"]["rolled_back"], 1);
    }

    #[tokio::test]
    async fn omitted_empty_ssh_opts_deserializes_and_reaches_effects() {
        let settings = minimal_parent_deploy_settings();
        let parsed = serde_json::from_value::<FlattenedDeploySettings>(settings.clone()).unwrap();
        assert!(parsed.ssh_opts.is_empty());

        let base = tmp_base("host-omitted-ssh-opts");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = prepare_run(&inv(), "cache", 4, true).unwrap();
        run.plan.settings.insert("cache".into(), settings);
        let (programs, trace) = effect_programs(&base, 0, 0);

        let result = deploy_host_with(&run, &built_cache_profile(), "cache", &programs).await;
        assert_eq!(result.state, HostState::Confirmed);
        assert_eq!(result.error, None);

        let trace = std::fs::read_to_string(trace).unwrap();
        assert!(trace.contains(
            "NIX_SSHOPTS=-p 22 -i /Users/bryan/.ssh/id_rsa -o IdentitiesOnly=yes -o IdentityAgent=none args=copy"
        ));
        assert!(trace.contains(
            "args=root@172.16.42.7 -p 22 -i /Users/bryan/.ssh/id_rsa -o IdentitiesOnly=yes -o IdentityAgent=none"
        ));
    }

    #[tokio::test]
    async fn dry_activate_skips_magic_wait_and_confirmation() {
        let base = tmp_base("host-dry");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = prepare_run(&inv(), "cache", 4, true).unwrap();
        run.plan.boot = true;
        run.plan
            .settings
            .insert("cache".into(), deploy_settings("switch"));
        let (programs, trace) = effect_programs(&base, 0, 0);

        let result = deploy_host_with(&run, &built_cache_profile(), "cache", &programs).await;
        assert_eq!(result.state, HostState::RebootPending);
        let trace = std::fs::read_to_string(trace).unwrap();
        assert!(!trace.contains("ssh USER="));
        assert!(!trace.contains("nix-env --profile"));
        assert!(!trace.contains(" activate-rs wait "));
        assert!(!trace.contains(" rm "));
    }

    #[tokio::test]
    async fn stale_canary_cleanup_finishes_before_activate_and_wait_spawn() {
        let base = tmp_base("host-stale-canary-cleanup");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = prepare_run(&inv(), "cache", 4, false).unwrap();
        run.plan
            .settings
            .insert("cache".into(), deploy_settings("switch"));
        let (programs, trace) = effect_programs(&base, 0, 0);

        let result = deploy_host_with(&run, &built_cache_profile(), "cache", &programs).await;
        assert_eq!(result.state, HostState::Confirmed);

        let trace = std::fs::read_to_string(trace).unwrap();
        let cleanup = trace.find(" rm -f ").unwrap();
        let activate = trace.find("activate-rs activate ").unwrap();
        let wait = trace.find("activate-rs wait ").unwrap();
        let cleanup_line = trace.lines().find(|line| line.contains(" rm -f ")).unwrap();
        assert!(cleanup_line.contains("doas -u app rm -f /run/mandala/deploy-rs-canary-"));
        assert!(cleanup_line.ends_with("00000000000000000000000000000000"));
        assert!(cleanup < activate);
        assert!(cleanup < wait);
    }

    #[tokio::test]
    async fn stale_canary_cleanup_failure_prevents_activate_and_wait() {
        let base = tmp_base("host-stale-canary-cleanup-failure");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = prepare_run(&inv(), "cache", 4, false).unwrap();
        run.plan
            .settings
            .insert("cache".into(), deploy_settings("switch"));
        let (programs, trace) = effect_programs_with_cleanup(&base, 0, 23, 0);

        let result = deploy_host_with(&run, &built_cache_profile(), "cache", &programs).await;
        assert_eq!(result.state, HostState::Failed);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("stale deployment canary cleanup failed with exit status: 23")
        );

        let trace = std::fs::read_to_string(trace).unwrap();
        assert!(trace.contains(" rm -f "));
        assert!(!trace.contains("activate-rs activate "));
        assert!(!trace.contains("activate-rs wait "));

        let mut tailer = crate::runner::EventTailer::new(&run.path);
        tailer.poll();
        assert_eq!(tailer.hosts["cache"].state, HostState::Failed);
        assert_eq!(
            tailer.hosts["cache"].milestones,
            ["copy", "activate", "wait"]
        );
    }

    #[tokio::test]
    async fn failed_confirmation_maps_to_sticky_rollback() {
        let base = tmp_base("host-rollback");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = prepare_run(&inv(), "cache", 4, false).unwrap();
        run.plan
            .settings
            .insert("cache".into(), deploy_settings("switch"));
        let (programs, trace) = effect_programs(&base, 0, 42);

        let result = deploy_host_with(&run, &built_cache_profile(), "cache", &programs).await;
        assert_eq!(result.state, HostState::RolledBack);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("confirmation failed")
        );
        let trace = std::fs::read_to_string(trace).unwrap();
        assert!(trace.contains("activate-rs wait '/nix/store/00000000000000000000000000000000-cache-profile' --temp-path '/run/mandala' --activation-timeout 97"));
        assert!(
            trace.contains("rm /run/mandala/deploy-rs-canary-00000000000000000000000000000000")
        );

        let mut tailer = crate::runner::EventTailer::new(&run.path);
        tailer.poll();
        assert_eq!(tailer.hosts["cache"].state, HostState::RolledBack);
        assert_eq!(tailer.hosts["cache"].rc, Some(1));
        assert_eq!(
            tailer.hosts["cache"].milestones,
            ["copy", "activate", "wait", "rollback"]
        );
    }

    #[tokio::test]
    async fn failed_confirmation_wins_over_post_rollback_activation_exit() {
        let base = tmp_base("host-real-rollback-exit");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = prepare_run(&inv(), "cache", 4, false).unwrap();
        run.plan
            .settings
            .insert("cache".into(), deploy_settings("switch"));
        let (programs, trace) =
            ordered_activation_programs(&base, 42, 1, ActivationEffectOrder::ConfirmationFirst);
        let task = program_task(BTreeMap::from([("cache".into(), programs)]));

        let outcome = fan_out_with(&run, &built_cache_profile(), task)
            .await
            .unwrap();
        let result = &outcome.results["cache"];
        assert_eq!(result.state, HostState::RolledBack);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("confirmation failed")
        );
        assert_eq!(outcome.summary.failed, 0);
        assert_eq!(outcome.summary.rolled_back, 1);
        assert_eq!(outcome.rc, 1);
        assert!(std::fs::read_to_string(trace).unwrap().contains(" rm "));

        let mut tailer = crate::runner::EventTailer::new(&run.path);
        tailer.poll();
        assert_eq!(tailer.hosts["cache"].state, HostState::RolledBack);
        assert_eq!(
            tailer.hosts["cache"].milestones,
            ["copy", "activate", "wait", "rollback"]
        );
        let meta = registry::read_meta(&run.path);
        assert_eq!(meta["summary"]["failed"], 0);
        assert_eq!(meta["summary"]["rolled_back"], 1);
    }

    #[tokio::test]
    async fn successful_confirmation_does_not_hide_activation_failure() {
        let base = tmp_base("host-activation-exit");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = prepare_run(&inv(), "cache", 4, false).unwrap();
        run.plan
            .settings
            .insert("cache".into(), deploy_settings("switch"));
        let (programs, trace) =
            ordered_activation_programs(&base, 0, 1, ActivationEffectOrder::ConfirmationFirst);

        let result = deploy_host_with(&run, &built_cache_profile(), "cache", &programs).await;
        assert_eq!(result.state, HostState::Failed);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("ssh activation failed with exit status: 1")
        );
        assert!(std::fs::read_to_string(trace).unwrap().contains(" rm "));

        let mut tailer = crate::runner::EventTailer::new(&run.path);
        tailer.poll();
        assert_eq!(tailer.hosts["cache"].state, HostState::Failed);
        assert_eq!(
            tailer.hosts["cache"].milestones,
            ["copy", "activate", "wait"]
        );
    }

    #[tokio::test]
    async fn post_wait_activation_exit_wins_over_late_confirmation_success() {
        let base = tmp_base("host-late-confirmation");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = prepare_run(&inv(), "cache", 4, false).unwrap();
        run.plan
            .settings
            .insert("cache".into(), deploy_settings("switch"));
        let (programs, trace) = ordered_activation_programs(
            &base,
            0,
            1,
            ActivationEffectOrder::ConfirmationAfterActivation,
        );
        let task = program_task(BTreeMap::from([("cache".into(), programs)]));

        let outcome = fan_out_with(&run, &built_cache_profile(), task)
            .await
            .unwrap();
        let result = &outcome.results["cache"];
        assert_eq!(result.state, HostState::RolledBack);
        assert!(
            result.error.as_deref().unwrap().contains(
                "activation rolled back with exit status: 1 before confirmation completed"
            )
        );
        assert_eq!(outcome.summary.failed, 0);
        assert_eq!(outcome.summary.rolled_back, 1);
        assert!(std::fs::read_to_string(trace).unwrap().contains(" rm "));

        let mut tailer = crate::runner::EventTailer::new(&run.path);
        tailer.poll();
        assert_eq!(tailer.hosts["cache"].state, HostState::RolledBack);
        assert_eq!(
            tailer.hosts["cache"].milestones,
            ["copy", "activate", "wait", "rollback"]
        );
        let meta = registry::read_meta(&run.path);
        assert_eq!(meta["summary"]["failed"], 0);
        assert_eq!(meta["summary"]["rolled_back"], 1);
    }

    #[tokio::test]
    async fn activation_failure_before_waiter_completion_remains_failed() {
        let base = tmp_base("host-pre-wait-failure");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = prepare_run(&inv(), "cache", 4, false).unwrap();
        run.plan
            .settings
            .insert("cache".into(), deploy_settings("switch"));
        let (programs, trace) =
            ordered_activation_programs(&base, 0, 1, ActivationEffectOrder::ActivationBeforeWait);

        let result = deploy_host_with(&run, &built_cache_profile(), "cache", &programs).await;
        assert_eq!(result.state, HostState::Failed);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("ssh activation failed with exit status: 1")
        );
        let trace = std::fs::read_to_string(trace).unwrap();
        assert!(trace.contains(" rm -f "));
        assert!(
            !trace
                .lines()
                .any(|line| line.contains(" rm ") && !line.contains(" rm -f "))
        );

        let mut tailer = crate::runner::EventTailer::new(&run.path);
        tailer.poll();
        assert_eq!(tailer.hosts["cache"].state, HostState::Failed);
    }

    #[tokio::test]
    async fn copy_failure_maps_to_failed_without_activation() {
        let base = tmp_base("host-failed");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = prepare_run(&inv(), "cache", 4, false).unwrap();
        run.plan
            .settings
            .insert("cache".into(), deploy_settings("switch"));
        let (programs, trace) = effect_programs(&base, 17, 0);

        let result = deploy_host_with(&run, &built_cache_profile(), "cache", &programs).await;
        assert_eq!(result.state, HostState::Failed);
        assert!(result.error.as_deref().unwrap().contains("nix copy failed"));
        let trace = std::fs::read_to_string(trace).unwrap();
        assert!(!trace.contains("ssh USER="));

        let mut tailer = crate::runner::EventTailer::new(&run.path);
        tailer.poll();
        assert_eq!(tailer.hosts["cache"].state, HostState::Failed);
        assert_eq!(tailer.hosts["cache"].rc, Some(1));
        assert_eq!(tailer.hosts["cache"].milestones, ["copy"]);
    }

    #[tokio::test]
    async fn fan_out_bounds_concurrency_and_eventually_completes_every_host() {
        let base = tmp_base("fanout-bound");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let run = configured_k3s_run(&base, 1);
        let (programs, trace) = effect_programs(&base, 0, 0);
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let maximum_check = Arc::clone(&maximum);
        let completed_check = Arc::clone(&completed);
        let task: HostTask = Arc::new(move |run, built, host| {
            let programs = programs.clone();
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            let completed = Arc::clone(&completed);
            Box::pin(async move {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                let result = deploy_host_with(&run, &built, &host, &programs).await;
                active.fetch_sub(1, Ordering::SeqCst);
                completed.fetch_add(1, Ordering::SeqCst);
                result
            })
        });

        let outcome = fan_out_with(&run, &built_k3s_profiles(), task)
            .await
            .unwrap();
        assert_eq!(maximum_check.load(Ordering::SeqCst), 1);
        assert_eq!(completed_check.load(Ordering::SeqCst), 2);
        assert_eq!(outcome.summary.total, 2);
        assert_eq!(outcome.summary.confirmed, 2);
        assert_eq!(outcome.rc, 0);
        assert_eq!(outcome.process_rc, 0);
        let trace = std::fs::read_to_string(trace).unwrap();
        assert_eq!(
            trace
                .lines()
                .filter(|line| line.starts_with("nix "))
                .count(),
            2
        );
        assert_eq!(
            trace
                .lines()
                .filter(|line| line.starts_with("ssh "))
                .count(),
            6
        );

        let meta = registry::read_meta(&run.path);
        assert_eq!(meta["build_rc"], 0);
        assert_eq!(meta["process_rc"], 0);
        assert_eq!(meta["rc"], 0);
        assert_eq!(
            meta["summary"],
            json!({
                "total": 2,
                "confirmed": 2,
                "reboot_pending": 0,
                "failed": 0,
                "rolled_back": 0
            })
        );
        assert!(meta["finished_at"].as_f64().is_some());
    }

    #[tokio::test]
    async fn partial_failure_is_loud_and_does_not_revoke_successful_sibling() {
        let base = tmp_base("fanout-partial");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let run = configured_k3s_run(&base, 2);
        let (failed_programs, failed_trace) = effect_programs(&base, 17, 0);
        let (healthy_programs, healthy_trace) = effect_programs(&base, 0, 0);
        let task = program_task(BTreeMap::from([
            ("cache".into(), failed_programs),
            ("web".into(), healthy_programs),
        ]));

        let outcome = fan_out_with(&run, &built_k3s_profiles(), task)
            .await
            .unwrap();
        assert_eq!(outcome.results["cache"].state, HostState::Failed);
        assert_eq!(outcome.results["web"].state, HostState::Confirmed);
        assert_eq!(outcome.summary.failed, 1);
        assert_eq!(outcome.summary.confirmed, 1);
        assert_eq!(outcome.process_rc, 0);
        assert_eq!(outcome.rc, 1);
        let lines = outcome_lines(&outcome);
        assert!(lines.iter().any(|line| line.contains("cache: failed")));
        assert!(lines.iter().any(|line| line.contains("PARTIAL FAILURE")));
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("all hosts confirmed"))
        );
        assert!(
            !std::fs::read_to_string(failed_trace)
                .unwrap()
                .contains("revoke")
        );
        let healthy_trace = std::fs::read_to_string(healthy_trace).unwrap();
        assert!(healthy_trace.contains("ssh USER="));
        assert!(!healthy_trace.contains("revoke"));

        let meta = registry::read_meta(&run.path);
        assert_eq!(meta["build_rc"], 0);
        assert_eq!(meta["process_rc"], 0);
        assert_eq!(meta["rc"], 1);
        assert_eq!(meta["summary"]["failed"], 1);
        assert_eq!(meta["summary"]["confirmed"], 1);
    }

    #[tokio::test]
    async fn rolled_back_host_does_not_revoke_confirmed_sibling() {
        let base = tmp_base("fanout-rollback");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let mut run = configured_k3s_run(&base, 2);
        run.plan.dry_activate = false;
        let (rollback_programs, rollback_trace) = effect_programs(&base, 0, 42);
        let (healthy_programs, healthy_trace) = effect_programs(&base, 0, 0);
        let task = program_task(BTreeMap::from([
            ("cache".into(), rollback_programs),
            ("web".into(), healthy_programs),
        ]));

        let outcome = fan_out_with(&run, &built_k3s_profiles(), task)
            .await
            .unwrap();
        assert_eq!(outcome.results["cache"].state, HostState::RolledBack);
        assert_eq!(outcome.results["web"].state, HostState::Confirmed);
        assert_eq!(outcome.summary.rolled_back, 1);
        assert_eq!(outcome.summary.confirmed, 1);
        assert_eq!(outcome.rc, 1);
        assert!(
            !std::fs::read_to_string(rollback_trace)
                .unwrap()
                .contains("revoke")
        );
        assert!(
            !std::fs::read_to_string(healthy_trace)
                .unwrap()
                .contains("revoke")
        );

        let mut tailer = crate::runner::EventTailer::new(&run.path);
        tailer.poll();
        assert_eq!(tailer.hosts["cache"].state, HostState::RolledBack);
        assert_eq!(tailer.hosts["web"].state, HostState::Confirmed);
        let meta = registry::read_meta(&run.path);
        assert_eq!(meta["process_rc"], 0);
        assert_eq!(meta["rc"], 1);
        assert_eq!(meta["summary"]["rolled_back"], 1);
    }

    #[tokio::test]
    async fn panicking_host_is_failed_and_sibling_still_completes() {
        let base = tmp_base("fanout-panic");
        let _guard = registry::test_hooks::install_runs_base(base.clone());
        let run = configured_k3s_run(&base, 2);
        let (programs, healthy_trace) = effect_programs(&base, 0, 0);
        let task: HostTask = Arc::new(move |run, built, host| {
            let programs = programs.clone();
            Box::pin(async move {
                assert_ne!(host, "cache", "deliberate per-host panic");
                deploy_host_with(&run, &built, &host, &programs).await
            })
        });

        let outcome = fan_out_with(&run, &built_k3s_profiles(), task)
            .await
            .unwrap();
        assert_eq!(outcome.results["cache"].state, HostState::Failed);
        assert!(
            outcome.results["cache"]
                .error
                .as_deref()
                .unwrap()
                .contains("panicked")
        );
        assert_eq!(outcome.results["web"].state, HostState::Confirmed);
        assert_eq!(outcome.summary.failed, 1);
        assert_eq!(outcome.summary.confirmed, 1);
        assert_eq!(outcome.rc, 1);
        assert!(
            std::fs::read_to_string(healthy_trace)
                .unwrap()
                .contains("ssh USER=")
        );

        let mut tailer = crate::runner::EventTailer::new(&run.path);
        tailer.poll();
        assert_eq!(tailer.hosts["cache"].state, HostState::Failed);
        assert_eq!(tailer.hosts["web"].state, HostState::Confirmed);
        assert_eq!(registry::read_meta(&run.path)["rc"], 1);
    }

    #[test]
    fn batch_argv_validates_group() {
        assert_eq!(
            batch_argv(&inv(), ".", "k3s").unwrap(),
            vec![
                "nix",
                "build",
                "--no-link",
                "--print-out-paths",
                ".#deployBatch.k3s",
            ]
        );
        // `all` is always allowed even though it is not a named group.
        assert_eq!(
            batch_argv(&inv(), "/flake", "all").unwrap().last().unwrap(),
            "/flake#deployBatch.all"
        );
        // Unknown group errors enumerate the available groups.
        let err = batch_argv(&inv(), ".", "nope").unwrap_err();
        assert_eq!(
            err.to_string(),
            "no such group: nope (available: fleet, gateway, k3s)"
        );
    }

    #[test]
    fn exit_byte_clamps() {
        assert_eq!(exit_byte(Some(0)), 0);
        assert_eq!(exit_byte(Some(2)), 2);
        assert_eq!(exit_byte(None), 1);
        assert_eq!(exit_byte(Some(256)), 1); // 256 & 0xff == 0 → floored to 1
    }
}
