//! Owner-enforced, bounded SSH stdio transaction protocol. There is no listener.
//! A session owns the OS lock until settlement or loss; durable unknown state
//! outlives that lock and prevents a subsequent controller from replaying work.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::manifest::{Manifest, Observation};
use super::{OwnerUnit, RECOVERY_LIMIT};

mod client;
mod native;
pub use client::OwnerClient;
pub use native::NativeEffects;

/// Binary entry point: fixed private state root, no configurable command or
/// listener, and the controller's same owned-child termination behavior.
///
/// # Errors
/// Signal/runtime initialization, protocol or owner transaction failure.
pub fn run_stdio() -> io::Result<()> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Config {
        owner: String,
    }
    let config: Config = serde_json::from_slice(&std::fs::read("/etc/mandala/vm-owner.json")?)
        .map_err(io::Error::other)?;
    let _signals = mandala_deploy::process::SignalForwarder::install(true)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(serve(
            &mut tokio::io::BufReader::new(tokio::io::stdin()),
            &mut tokio::io::stdout(),
            Path::new("/var/lib/mandala/vm"),
            &config.owner,
            &NativeEffects::default(),
            Duration::from_secs(5),
        ))
}

pub const VERSION: u64 = 1;
pub const MAX_MESSAGE: u64 = 1024 * 1024;
pub const SESSION_SECONDS: u64 = 4 * 60 * 60;
const IDLE_SECONDS: u64 = 120;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    Hello {
        version: u64,
    },
    Lock {
        owner: String,
        transaction: String,
        dry: bool,
        reconcile: bool,
    },
    Prepare {
        unit: Box<OwnerUnit>,
        manifest: Box<Manifest>,
        profile: String,
    },
    HostStart,
    AbortUnapplied {
        diagnostic: String,
    },
    Apply {
        mode: Mode,
    },
    Recover {
        auto_rollback: bool,
        primary: String,
    },
    Reconcile {
        restore: bool,
    },
    Finish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Live,
    Boot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GuestOutcome {
    Confirmed,
    Pending,
    Preview,
    Restored,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Locked,
    Prepared,
    Mutating,
    Confirmed,
    Pending,
    Restored,
    FailedUnrecovered,
    PartialRecovery,
    Unknown,
    NotApplied,
}
impl State {
    fn settled(self) -> bool {
        matches!(
            self,
            Self::Confirmed | Self::Pending | Self::Restored | Self::NotApplied
        )
    }
}

/// Exact persisted intended, previous and observed evidence. Old/unknown journal
/// versions fail closed; no host-profile-only reconstruction is permitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Journal {
    pub version: u64,
    pub owner: String,
    pub transaction: String,
    pub state: State,
    pub unit: Option<OwnerUnit>,
    pub manifest: Option<Manifest>,
    pub profile: Option<String>,
    pub previous: Option<Observation>,
    pub observed: Option<Observation>,
    pub guests: BTreeMap<String, GuestOutcome>,
    pub primary_failure: Option<String>,
    pub recovery_failure: Option<String>,
    pub recovery_limit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Reply {
    pub version: u64,
    pub ok: bool,
    pub error: Option<String>,
    pub journal: Option<Journal>,
}
impl Reply {
    fn success(journal: Option<Journal>) -> Self {
        Self {
            version: VERSION,
            ok: true,
            error: None,
            journal,
        }
    }
    fn failure(error: impl ToString, journal: Option<Journal>) -> Self {
        Self {
            version: VERSION,
            ok: false,
            error: Some(error.to_string()),
            journal,
        }
    }
    /// Check both directions of the protocol before trusting any evidence.
    ///
    /// # Errors
    /// Unsupported helper version, or a helper-side protocol/effect failure.
    pub fn check(self) -> io::Result<Option<Journal>> {
        if self.version != VERSION {
            return Err(invalid(format!(
                "VM owner protocol mismatch: controller {VERSION}, helper {}",
                self.version
            )));
        }
        if !self.ok {
            return Err(invalid(
                self.error
                    .unwrap_or_else(|| "owner helper refused request".into()),
            ));
        }
        Ok(self.journal)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Operation {
    Inspect,
    Prepare,
    Apply,
    Verify,
    Restore,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationTarget {
    Desired,
    Previous,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdapterRequest {
    pub version: u64,
    pub operation: Operation,
    pub target: VerificationTarget,
    pub manifest: Manifest,
    pub profile: String,
    pub previous: Option<Observation>,
    pub mode: Mode,
}
/// A successful exit without this typed evidence is never sufficient.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
pub enum AdapterEvidence {
    Inspect {
        observation: Observation,
    },
    Prepare {
        supported_systems: Vec<String>,
        runtime_instances: BTreeMap<String, String>,
        available_paths: BTreeSet<String>,
    },
    Apply {
        dispositions: BTreeMap<String, GuestOutcome>,
    },
    Verify {
        observation: Observation,
    },
    Restore {
        observation: Observation,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdapterReply {
    pub version: u64,
    pub evidence: AdapterEvidence,
}

/// Injected effects; conformance tests use only local fixtures and stub runtimes.
pub trait Effects: Send + Sync {
    fn adapter(
        &self,
        request: AdapterRequest,
    ) -> impl std::future::Future<Output = io::Result<AdapterReply>> + Send;
    fn closure(
        &self,
        profile: &str,
        expected: &Manifest,
    ) -> impl std::future::Future<Output = io::Result<BTreeSet<String>>> + Send;
    fn root(
        &self,
        path: &str,
        link: &Path,
    ) -> impl std::future::Future<Output = io::Result<()>> + Send;
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

struct LockedStore {
    directory: PathBuf,
    _lock: File,
}
impl LockedStore {
    async fn acquire(root: &Path, owner: &str, bound: Duration) -> io::Result<Self> {
        if !crate::inventory::is_valid_member_name(owner) {
            return Err(invalid("invalid owner identity"));
        }
        let directory = root.join(owner);
        std::fs::create_dir_all(root)?;
        match std::fs::create_dir(&directory) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e),
        }
        if !std::fs::symlink_metadata(&directory)?.is_dir() {
            return Err(invalid("owner state directory is not a real directory"));
        }
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(directory.join("lock"))?;
        let start = tokio::time::Instant::now();
        loop {
            match lock.try_lock() {
                Ok(()) => {
                    return Ok(Self {
                        directory,
                        _lock: lock,
                    });
                }
                Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < bound => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(invalid(format!(
                        "owner {owner} busy: lock acquisition bound expired"
                    )));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
            }
        }
    }
    fn read(&self) -> io::Result<Option<Journal>> {
        let path = self.directory.join("journal.json");
        match std::fs::read(path) {
            Ok(bytes) => {
                let journal: Journal = serde_json::from_slice(&bytes)
                    .map_err(|e| invalid(format!("unreadable owner recovery journal: {e}")))?;
                if journal.version != VERSION {
                    return Err(invalid(format!(
                        "VM journal version mismatch: helper {VERSION}, journal {}",
                        journal.version
                    )));
                }
                Ok(Some(journal))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn write(&self, journal: &Journal) -> io::Result<()> {
        let tmp = self.directory.join("journal.tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(&tmp)?;
        serde_json::to_writer(&mut file, journal).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&tmp, self.directory.join("journal.json"))?;
        File::open(&self.directory)?.sync_all()
    }
}

struct Session<'a, E> {
    expected_owner: &'a str,
    root: &'a Path,
    effects: &'a E,
    lock_bound: Duration,
    store: Option<LockedStore>,
    journal: Option<Journal>,
    dry: bool,
    reconciling: bool,
    finished: bool,
    poisoned: bool,
}
impl<E: Effects> Session<'_, E> {
    fn persist(&mut self) -> io::Result<()> {
        let result = self.persist_inner();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    fn persist_inner(&self) -> io::Result<()> {
        if !self.dry {
            self.store
                .as_ref()
                .ok_or_else(|| invalid("owner lock is required"))?
                .write(
                    self.journal
                        .as_ref()
                        .ok_or_else(|| invalid("missing recovery journal"))?,
                )?;
        }
        Ok(())
    }
    async fn adapter(&self, operation: Operation, mode: Mode) -> io::Result<AdapterEvidence> {
        self.adapter_target(operation, mode, VerificationTarget::Desired)
            .await
    }
    async fn adapter_target(
        &self,
        operation: Operation,
        mode: Mode,
        target: VerificationTarget,
    ) -> io::Result<AdapterEvidence> {
        let journal = self
            .journal
            .as_ref()
            .ok_or_else(|| invalid("owner lock is required"))?;
        let manifest = journal
            .manifest
            .clone()
            .ok_or_else(|| invalid("prepare is required"))?;
        let bound = manifest
            .guests
            .values()
            .map(|g| g.readiness.deadline_seconds)
            .max()
            .unwrap_or(1)
            + 5;
        let result = tokio::time::timeout(
            Duration::from_secs(bound),
            self.effects.adapter(AdapterRequest {
                version: VERSION,
                operation,
                target,
                manifest,
                profile: journal
                    .profile
                    .clone()
                    .ok_or_else(|| invalid("missing desired owner profile"))?,
                previous: journal.previous.clone(),
                mode,
            }),
        )
        .await
        .map_err(|_| invalid("owner adapter deadline expired; runtime observation unknown"))??;
        if result.version != VERSION {
            return Err(invalid(format!(
                "VM adapter protocol mismatch: helper {VERSION}, adapter {}",
                result.version
            )));
        }
        Ok(result.evidence)
    }
    async fn request(&mut self, request: Request) -> io::Result<()> {
        if self.poisoned {
            return Err(invalid(
                "owner journal persistence failed; no further effects permitted",
            ));
        }
        match request {
            Request::Hello { .. } => return Err(invalid("handshake already completed")),
            Request::Lock {
                owner,
                transaction,
                dry,
                reconcile,
            } => {
                if owner != self.expected_owner {
                    return Err(invalid(
                        "requested owner does not match the owner module identity",
                    ));
                }
                if self.store.is_some() || !valid_id(&transaction) {
                    return Err(invalid("duplicate lock or invalid transaction identity"));
                }
                let store = LockedStore::acquire(self.root, &owner, self.lock_bound).await?;
                let mut previous = store.read()?;
                if !dry
                    && let Some(stale) = previous.as_mut()
                    && matches!(
                        stale.state,
                        State::Locked | State::Prepared | State::Mutating
                    )
                {
                    stale.state = State::Unknown;
                    stale.primary_failure.get_or_insert_with(|| {
                        "prior owner session ended without verified settlement".into()
                    });
                    store.write(stale)?;
                }
                if let Some(previous) = &previous {
                    if previous.owner != owner {
                        return Err(invalid("owner journal identity mismatch"));
                    }
                    if !previous.state.settled() && !reconcile {
                        return Err(invalid(
                            "owner runtime unknown/recovery-required; explicit reconciliation required",
                        ));
                    }
                }
                if reconcile && (dry || previous.as_ref().is_none_or(|j| j.state.settled())) {
                    return Err(invalid("no unsettled owner transaction to reconcile"));
                }
                self.dry = dry;
                self.reconciling = reconcile;
                self.store = Some(store);
                self.journal = if reconcile {
                    previous
                } else {
                    Some(Journal {
                        version: VERSION,
                        owner,
                        transaction,
                        state: State::Locked,
                        unit: None,
                        manifest: None,
                        profile: None,
                        previous: None,
                        observed: None,
                        guests: BTreeMap::new(),
                        primary_failure: None,
                        recovery_failure: None,
                        recovery_limit: RECOVERY_LIMIT.into(),
                    })
                };
                self.persist()?;
            }
            Request::Prepare {
                unit,
                manifest,
                profile,
            } => {
                let journal = self
                    .journal
                    .as_ref()
                    .ok_or_else(|| invalid("owner lock is required before copy/prepare"))?;
                if self.reconciling || journal.state != State::Locked || journal.owner != unit.owner
                {
                    return Err(invalid("prepare owner/transaction mismatch"));
                }
                let closure = self.effects.closure(&profile, &manifest).await?;
                manifest.validate(&unit, &closure).map_err(invalid)?;
                {
                    let journal = self.journal.as_mut().expect("checked journal");
                    journal.unit = Some(*unit);
                    journal.manifest = Some(*manifest);
                    journal.profile = Some(profile);
                }
                let AdapterEvidence::Inspect { observation } =
                    self.adapter(Operation::Inspect, Mode::Live).await?
                else {
                    return Err(invalid("inspect returned wrong evidence type"));
                };
                self.journal
                    .as_ref()
                    .unwrap()
                    .manifest
                    .as_ref()
                    .unwrap()
                    .validate_previous(&observation)
                    .map_err(invalid)?;
                self.journal.as_mut().unwrap().previous = Some(observation);
                let AdapterEvidence::Prepare {
                    supported_systems,
                    runtime_instances,
                    available_paths,
                } = self.adapter(Operation::Prepare, Mode::Live).await?
                else {
                    return Err(invalid("prepare returned wrong evidence type"));
                };
                let journal = self.journal.as_ref().unwrap();
                let manifest = journal.manifest.as_ref().unwrap();
                let expected_instances: BTreeMap<_, _> = manifest
                    .guests
                    .iter()
                    .map(|(n, g)| (n.clone(), g.runtime_instance.clone()))
                    .collect();
                if supported_systems != manifest.supported_systems
                    || runtime_instances != expected_instances
                    || !manifest
                        .guests
                        .values()
                        .all(|g| available_paths.contains(&g.artifact))
                {
                    return Err(invalid(
                        "prepare evidence lacks complete paths or qualified system/runtime pairs",
                    ));
                }
                if !self.dry {
                    let previous = journal.previous.as_ref().unwrap();
                    let mut roots = BTreeSet::from([
                        journal.profile.clone().unwrap(),
                        previous.owner_profile.clone(),
                        previous.owner_running.clone(),
                        previous.boot_default.clone(),
                        previous
                            .adapter
                            .split('/')
                            .take(4)
                            .collect::<Vec<_>>()
                            .join("/"),
                    ]);
                    for runtime in previous.guests.values() {
                        roots.insert(runtime.artifact.clone());
                        for reference in [&runtime.readiness.contract, &runtime.recovery_contract] {
                            roots
                                .insert(reference.split('/').take(4).collect::<Vec<_>>().join("/"));
                        }
                    }
                    let directory = self
                        .store
                        .as_ref()
                        .unwrap()
                        .directory
                        .join("roots")
                        .join(&journal.transaction);
                    std::fs::create_dir_all(&directory)?;
                    for (index, path) in roots.iter().enumerate() {
                        self.effects
                            .root(path, &directory.join(index.to_string()))
                            .await?;
                    }
                }
                let journal = self.journal.as_mut().unwrap();
                journal.state = State::Prepared;
                journal.guests = journal
                    .manifest
                    .as_ref()
                    .unwrap()
                    .guests
                    .keys()
                    .map(|n| {
                        (
                            n.clone(),
                            if self.dry {
                                GuestOutcome::Preview
                            } else {
                                GuestOutcome::Unknown
                            },
                        )
                    })
                    .collect();
                self.persist()?;
            }
            Request::AbortUnapplied { diagnostic } => {
                let journal = self
                    .journal
                    .as_mut()
                    .ok_or_else(|| invalid("owner lock is required"))?;
                if self.reconciling || !matches!(journal.state, State::Locked | State::Prepared) {
                    return Err(invalid(
                        "cannot abort a possibly mutated owner as unapplied",
                    ));
                }
                journal.state = State::NotApplied;
                journal.primary_failure = Some(diagnostic);
                self.persist()?;
            }
            Request::HostStart => {
                let journal = self
                    .journal
                    .as_mut()
                    .ok_or_else(|| invalid("prepare is required"))?;
                if self.dry || self.reconciling || journal.state != State::Prepared {
                    return Err(invalid(
                        "host mutation requires a prepared durable transaction",
                    ));
                }
                journal.state = State::Mutating;
                self.persist()?;
            }
            Request::Apply { mode } => {
                if self.dry
                    || self.reconciling
                    || self
                        .journal
                        .as_ref()
                        .is_none_or(|j| j.state != State::Mutating)
                {
                    return Err(invalid(
                        "runtime apply requires recorded host mutation intent",
                    ));
                }
                let AdapterEvidence::Apply { dispositions } =
                    self.adapter(Operation::Apply, mode).await?
                else {
                    return Err(invalid("apply returned wrong evidence type"));
                };
                let journal = self.journal.as_ref().unwrap();
                if dispositions
                    .keys()
                    .ne(journal.manifest.as_ref().unwrap().guests.keys())
                    || dispositions.values().any(|s| {
                        *s != if mode == Mode::Boot {
                            GuestOutcome::Pending
                        } else {
                            GuestOutcome::Confirmed
                        }
                    })
                {
                    return Err(invalid(
                        "runtime apply did not complete requested disposition",
                    ));
                }
                self.observe_desired(mode).await?;
            }
            Request::Recover {
                auto_rollback,
                primary,
            } => {
                if self.dry
                    || self.journal.as_ref().is_none_or(|j| {
                        j.previous.is_none()
                            || !matches!(
                                j.state,
                                State::Mutating
                                    | State::FailedUnrecovered
                                    | State::PartialRecovery
                                    | State::Unknown
                            )
                    })
                {
                    return Err(invalid("no recoverable unsettled transaction"));
                }
                self.journal.as_mut().unwrap().primary_failure = Some(primary);
                if !auto_rollback {
                    self.journal.as_mut().unwrap().state = State::FailedUnrecovered;
                    self.persist()?;
                } else {
                    self.restore().await?;
                }
            }
            Request::Reconcile { restore } => {
                if !self.reconciling {
                    return Err(invalid("explicit reconciliation lock is required"));
                }
                if self.journal.as_ref().is_none_or(|j| j.previous.is_none()) {
                    return Err(invalid(
                        "no persisted previous runtime; explicit operator reconciliation required",
                    ));
                }
                if restore {
                    self.restore().await?;
                } else {
                    self.observe_desired(Mode::Live).await?;
                }
            }
            Request::Finish => {
                if self.journal.as_ref().is_none_or(|j| {
                    !(j.state.settled()
                        || matches!(j.state, State::FailedUnrecovered | State::PartialRecovery)
                        || self.dry && j.state == State::Prepared)
                }) {
                    return Err(invalid(
                        "owner transaction is unsettled; reconciliation required",
                    ));
                }
                self.finished = true;
            }
        }
        Ok(())
    }
    async fn observe_desired(&mut self, mode: Mode) -> io::Result<()> {
        let AdapterEvidence::Verify { observation } = self.adapter(Operation::Verify, mode).await?
        else {
            return Err(invalid("verify returned wrong evidence type"));
        };
        let journal = self.journal.as_mut().unwrap();
        let manifest = journal.manifest.as_ref().unwrap();
        let previous = journal.previous.as_ref().unwrap();
        let mut good = observation.guests.keys().eq(manifest.guests.keys());
        journal.guests.clear();
        for (name, desired) in &manifest.guests {
            let matches = observation.guests.get(name).is_some_and(|g| {
                let expected = &previous.guests[name];
                g.ready
                    && g.runtime_instance == desired.runtime_instance
                    && if mode == Mode::Live {
                        g.readiness == desired.readiness
                            && g.recovery_contract == desired.recovery_contract
                            && g.running_identity == desired.desired_identity
                            && g.artifact == desired.artifact
                    } else {
                        g.readiness == expected.readiness
                            && g.recovery_contract == expected.recovery_contract
                            && g.running_identity == expected.running_identity
                            && g.artifact == expected.artifact
                    }
            });
            good &= matches;
            journal.guests.insert(
                name.clone(),
                if matches {
                    if mode == Mode::Live {
                        GuestOutcome::Confirmed
                    } else {
                        GuestOutcome::Pending
                    }
                } else {
                    GuestOutcome::Failed
                },
            );
        }
        good &= if mode == Mode::Live {
            observation.owner_profile == *journal.profile.as_ref().expect("prepared owner profile")
                && observation.owner_running == manifest.owner_generation
        } else {
            observation.boot_default == manifest.owner_generation
                && observation.owner_profile
                    == *journal.profile.as_ref().expect("prepared owner profile")
                && observation.owner_running == previous.owner_running
        };
        journal.observed = Some(observation);
        journal.state = if good {
            if mode == Mode::Live {
                State::Confirmed
            } else {
                State::Pending
            }
        } else {
            State::FailedUnrecovered
        };
        self.persist()?;
        if good {
            Ok(())
        } else {
            Err(invalid(
                "running identity/readiness or owner state failed verification",
            ))
        }
    }
    async fn restore(&mut self) -> io::Result<()> {
        let result = self.restore_inner().await;
        if let Err(error) = &result {
            let journal = self.journal.as_mut().expect("recoverable transaction");
            if journal.state != State::PartialRecovery {
                journal.state = State::Unknown;
                journal.recovery_failure = Some(error.to_string());
            }
            self.persist()?;
        }
        result
    }
    async fn restore_inner(&mut self) -> io::Result<()> {
        let evidence = self.adapter(Operation::Restore, Mode::Live).await;
        let observation = match evidence {
            Ok(AdapterEvidence::Restore { observation }) => observation,
            Ok(_) => return Err(invalid("restore returned wrong evidence type")),
            Err(error) => {
                let journal = self.journal.as_mut().unwrap();
                journal.state = State::Unknown;
                journal.recovery_failure = Some(error.to_string());
                self.persist()?;
                return Err(error);
            }
        };
        // Restore's success is not verification; independently inspect via verify.
        self.journal.as_mut().unwrap().observed = Some(observation);
        let AdapterEvidence::Verify { observation } = self
            .adapter_target(Operation::Verify, Mode::Live, VerificationTarget::Previous)
            .await?
        else {
            return Err(invalid("recovery verify returned wrong evidence type"));
        };
        let journal = self.journal.as_mut().unwrap();
        let previous = journal
            .previous
            .as_ref()
            .ok_or_else(|| invalid("missing previous runtime"))?;
        let mut good = observation.owner_profile == previous.owner_profile
            && observation.owner_running == previous.owner_running
            && observation.boot_default == previous.boot_default
            && observation.guests.keys().eq(previous.guests.keys());
        journal.guests.clear();
        for (name, expected) in &previous.guests {
            let matches = observation.guests.get(name).is_some_and(|g| {
                g.ready
                    && g.runtime_instance == expected.runtime_instance
                    && g.running_identity == expected.running_identity
                    && g.artifact == expected.artifact
                    && g.readiness == expected.readiness
                    && g.recovery_contract == expected.recovery_contract
            });
            good &= matches;
            journal.guests.insert(
                name.clone(),
                if matches {
                    GuestOutcome::Restored
                } else {
                    GuestOutcome::Failed
                },
            );
        }
        journal.observed = Some(observation);
        journal.state = if good {
            State::Restored
        } else {
            State::PartialRecovery
        };
        journal.recovery_failure = if good {
            None
        } else {
            Some("previous owner/guest identity or readiness not restored".into())
        };
        self.persist()?;
        if good {
            Ok(())
        } else {
            Err(invalid(
                "partial runtime recovery; owner unit remains failed",
            ))
        }
    }
    fn lost(&mut self, diagnostic: &str) {
        if !self.dry
            && !self.finished
            && let Some(journal) = self.journal.as_mut()
            && !journal.state.settled()
        {
            journal.state = State::Unknown;
            journal
                .primary_failure
                .get_or_insert_with(|| diagnostic.into());
            let _ = self.persist();
        }
    }
}

async fn line<R: AsyncBufRead + Unpin>(input: &mut R) -> io::Result<Option<String>> {
    let mut line = String::new();
    let count = input.take(MAX_MESSAGE + 1).read_line(&mut line).await?;
    if count == 0 {
        return Ok(None);
    }
    if count as u64 > MAX_MESSAGE || !line.ends_with('\n') {
        return Err(invalid("owner protocol message is oversized or incomplete"));
    }
    Ok(Some(line))
}
async fn write_reply<W: AsyncWrite + Unpin>(output: &mut W, reply: Reply) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(&reply).map_err(io::Error::other)?;
    bytes.push(b'\n');
    output.write_all(&bytes).await?;
    output.flush().await
}

/// Serve exactly one bounded transaction on SSH stdio. A timeout/EOF never
/// launches detached recovery or replays an unverified command.
///
/// # Errors
/// Invalid protocol, unavailable journal/lock, transport loss or deadline.
pub async fn serve<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin, E: Effects>(
    input: &mut R,
    output: &mut W,
    root: &Path,
    expected_owner: &str,
    effects: &E,
    lock_bound: Duration,
) -> io::Result<()> {
    let hello = tokio::time::timeout(Duration::from_secs(IDLE_SECONDS), line(input))
        .await
        .map_err(|_| invalid("owner handshake deadline expired"))??;
    let hello: Request =
        serde_json::from_str(&hello.ok_or_else(|| invalid("missing owner protocol handshake"))?)
            .map_err(io::Error::other)?;
    match hello {
        Request::Hello { version: VERSION } => write_reply(output, Reply::success(None)).await?,
        Request::Hello { version } => {
            let error =
                format!("VM owner protocol mismatch: controller {version}, helper {VERSION}");
            write_reply(output, Reply::failure(&error, None)).await?;
            return Err(invalid(error));
        }
        _ => {
            write_reply(
                output,
                Reply::failure("protocol handshake must be first", None),
            )
            .await?;
            return Err(invalid("protocol handshake must be first"));
        }
    }
    let mut session = Session {
        expected_owner,
        root,
        effects,
        lock_bound,
        store: None,
        journal: None,
        dry: false,
        reconciling: false,
        finished: false,
        poisoned: false,
    };
    let result = tokio::time::timeout(Duration::from_secs(SESSION_SECONDS), async {
        loop {
            let next = tokio::time::timeout(Duration::from_secs(IDLE_SECONDS), line(input)).await.map_err(|_| invalid("owner session idle deadline expired"))??;
            let request = next.ok_or_else(|| invalid("owner session lost; runtime outcome unknown"))?;
            let request: Request = serde_json::from_str(&request).map_err(io::Error::other)?;
            // A controller waits for each reply: EOF or a pipelined request
            // during an effect cancels that owned operation, never starts a
            // detached verify/restore after transport loss.
            let result = tokio::select! {
                result = session.request(request) => result,
                _ = line(input) => return Err(invalid("owner session lost or pipelined during operation; runtime unknown")),
            };
            let reply = match result { Ok(()) => Reply::success(session.journal.clone()), Err(error) => Reply::failure(error, session.journal.clone()) };
            write_reply(output, reply).await?;
            if session.finished { return Ok(()); }
        }
    }).await.unwrap_or_else(|_| Err(invalid("owner transaction session deadline expired")));
    if result.is_err() {
        session.lost("owner session lost/interrupted; explicit reconciliation required");
    }
    result
}

#[cfg(test)]
mod tests;
