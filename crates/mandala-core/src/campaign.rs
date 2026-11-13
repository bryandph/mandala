//! Durable campaign protocol and pure scheduling policy.
//!
//! This module never evaluates a flake, launches a process or activates a host.
//! Callers capture a frozen graph before binding, and publish prepared ownership
//! before allocating a normal registry run. All writes share an OS-backed lock;
//! losing a frontend releases the lock, not the uncertain attempt's ownership.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{drift, inventory, registry};

pub const SCHEMA_VERSION: u64 = 1;
const MANIFEST: &str = "campaign.json";

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Generated identifiers share the run timestamp grammar, with a distinct prefix.
#[must_use]
pub fn is_valid_id(id: &str) -> bool {
    id.strip_prefix("c-").is_some_and(registry::is_valid_run_id)
}

fn canonical<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    // Struct serialization otherwise preserves declaration order. Converting
    // through Value sorts every object, including nested settings and inputs.
    let value = serde_json::to_value(value).map_err(invalid_json)?;
    drift::to_pretty_1space(&value).map_err(invalid_json)
}

fn invalid_json(error: serde_json::Error) -> io::Error {
    invalid(error.to_string())
}

#[must_use]
pub fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn store_path(path: &str) -> bool {
    let Some(name) = path.strip_prefix("/nix/store/") else {
        return false;
    };
    !name.is_empty() && !name.contains('/') && !matches!(name, "." | "..")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceNode {
    pub store_path: String,
    pub content_digest: String,
    pub revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmoduleIdentity {
    pub gitlink_revision: String,
    pub checkout_revision: String,
    pub dirty: bool,
    pub content: SourceNode,
}

/// Full input paths are keys (for example `toolchain/nixpkgs`), not aliases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
    pub checkout: String,
    pub revision: Option<String>,
    pub dirty: bool,
    pub snapshot: SourceNode,
    pub lock_digest: String,
    pub inputs: BTreeMap<String, SourceNode>,
    pub submodules: BTreeMap<String, SubmoduleIdentity>,
}

impl SourceIdentity {
    /// Equality is about captured content, not mutable revision/dirty labels.
    #[must_use]
    pub fn same_content(&self, other: &Self) -> bool {
        self.snapshot.content_digest == other.snapshot.content_digest
            && self.lock_digest == other.lock_digest
            && self.inputs.len() == other.inputs.len()
            && self.inputs.iter().all(|(name, node)| {
                other
                    .inputs
                    .get(name)
                    .is_some_and(|observed| node.content_digest == observed.content_digest)
            })
            && self.submodules.len() == other.submodules.len()
            && self.submodules.iter().all(|(name, module)| {
                other.submodules.get(name).is_some_and(|observed| {
                    module.content.content_digest == observed.content.content_digest
                        && module.gitlink_revision == observed.gitlink_revision
                        && module.checkout_revision == observed.checkout_revision
                })
            })
    }

    fn validate(&self) -> io::Result<()> {
        if self.checkout.is_empty() || self.lock_digest.is_empty() {
            return Err(invalid("incomplete campaign source identity"));
        }
        for node in std::iter::once(&self.snapshot)
            .chain(self.inputs.values())
            .chain(self.submodules.values().map(|module| &module.content))
        {
            if !store_path(&node.store_path) || node.content_digest.is_empty() {
                return Err(invalid("unavailable campaign source content identity"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActivationMode {
    Switch,
    Boot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostTarget {
    pub profile: String,
    pub system_toplevel: String,
    pub derivation: String,
    pub output: String,
    pub settings: Value,
    pub activation: ActivationMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExclusionKind {
    Operator,
    NonDeployCapable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exclusion {
    pub reason: String,
    pub actor: String,
    pub timestamp: DateTime<Utc>,
    pub kind: ExclusionKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub selector: String,
    pub members: Vec<String>,
    pub deploy_members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub source: SourceIdentity,
    pub selection: Selection,
    pub hosts: BTreeMap<String, HostTarget>,
    pub initial_exclusions: BTreeMap<String, Exclusion>,
}

impl Target {
    pub fn digest(&self) -> io::Result<String> {
        Ok(digest(&canonical(self)?))
    }

    pub fn validate(&self) -> io::Result<()> {
        self.source.validate()?;
        let members: BTreeSet<_> = self.selection.members.iter().cloned().collect();
        let deploy: BTreeSet<_> = self.selection.deploy_members.iter().cloned().collect();
        if self.selection.selector.trim().is_empty()
            || members
                .iter()
                .any(|name| !inventory::is_valid_member_name(name))
            || self.selection.members != members.iter().cloned().collect::<Vec<_>>()
            || self.selection.deploy_members != deploy.iter().cloned().collect::<Vec<_>>()
            || !deploy.is_subset(&members)
            || deploy != self.hosts.keys().cloned().collect()
        {
            return Err(invalid(
                "campaign selection must be exact, sorted and unique",
            ));
        }
        for host in self.hosts.values() {
            if !store_path(&host.profile)
                || !store_path(&host.system_toplevel)
                || !store_path(&host.derivation)
                || !host.derivation.ends_with(".drv")
                || host.output.is_empty()
                || !host
                    .output
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || !host.settings.is_object()
            {
                return Err(invalid("unresolved or malformed campaign host target"));
            }
        }
        for (name, exclusion) in &self.initial_exclusions {
            validate_exclusion(exclusion)?;
            if !members.contains(name)
                || (exclusion.kind == ExclusionKind::NonDeployCapable && deploy.contains(name))
            {
                return Err(invalid("invalid initial campaign exclusion"));
            }
        }
        for name in members.difference(&deploy) {
            if !self
                .initial_exclusions
                .get(name)
                .is_some_and(|exclusion| exclusion.kind == ExclusionKind::NonDeployCapable)
            {
                return Err(invalid("non-deploy member requires a capability notice"));
            }
        }
        Ok(())
    }
}

fn validate_exclusion(exclusion: &Exclusion) -> io::Result<()> {
    if exclusion.reason.trim().is_empty() || exclusion.actor.trim().is_empty() {
        return Err(invalid("exclusions require actor and nonempty reason"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExclusionUpdate {
    pub generation: u64,
    pub timestamp: DateTime<Utc>,
    pub actor: String,
    pub reason: String,
    pub add: BTreeMap<String, String>,
    pub remove: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttemptKind {
    Deploy,
    Reboot,
    Verify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttemptState {
    Prepared,
    Running,
    Settled,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub number: u64,
    pub kind: AttemptKind,
    pub state: AttemptState,
    pub generation: u64,
    pub target_digest: String,
    pub targets: Vec<String>,
    pub run_id: Option<String>,
    pub dry_activate: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostOutcome {
    Pending,
    Failed,
    RolledBack,
    Confirmed,
    RebootPending,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeEvidence {
    pub outcome: HostOutcome,
    pub run_id: String,
    pub attempt: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lifecycle {
    Open,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Closure {
    pub closed_at: DateTime<Utc>,
    pub actor: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verification {
    pub observed_at: DateTime<Utc>,
    pub results: BTreeMap<String, Value>,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Campaign {
    pub campaign_id: String,
    pub schema_version: u64,
    pub created_at: DateTime<Utc>,
    pub target: Target,
    pub target_digest: String,
    pub generation: u64,
    pub lifecycle: Lifecycle,
    pub closure: Option<Closure>,
    pub exclusion_updates: Vec<ExclusionUpdate>,
    pub attempts: Vec<Attempt>,
    pub outcomes: BTreeMap<String, OutcomeEvidence>,
    pub verification: Vec<Verification>,
}

impl Campaign {
    pub fn new(id: String, target: Target, now: DateTime<Utc>) -> io::Result<Self> {
        if !is_valid_id(&id) {
            return Err(invalid("invalid campaign id"));
        }
        target.validate()?;
        let target_digest = target.digest()?;
        Ok(Self {
            campaign_id: id,
            schema_version: SCHEMA_VERSION,
            created_at: now,
            target,
            target_digest,
            generation: 0,
            lifecycle: Lifecycle::Open,
            closure: None,
            exclusion_updates: Vec::new(),
            attempts: Vec::new(),
            outcomes: BTreeMap::new(),
            verification: Vec::new(),
        })
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(invalid(format!(
                "unsupported campaign schema {}",
                self.schema_version
            )));
        }
        if !is_valid_id(&self.campaign_id) || self.target.digest()? != self.target_digest {
            return Err(invalid("corrupt campaign target identity"));
        }
        self.target.validate()?;
        if (self.lifecycle == Lifecycle::Closed) != self.closure.is_some() {
            return Err(invalid("campaign lifecycle and closure disagree"));
        }
        if let Some(closure) = &self.closure
            && (closure.actor.trim().is_empty() || closure.reason.trim().is_empty())
        {
            return Err(invalid("corrupt campaign closure attribution"));
        }
        let mut prior = 0;
        for update in &self.exclusion_updates {
            if update.generation <= prior || update.generation > self.generation {
                return Err(invalid("corrupt campaign exclusion history"));
            }
            prior = update.generation;
            if update.actor.trim().is_empty()
                || update.reason.trim().is_empty()
                || update.add.values().any(|reason| reason.trim().is_empty())
                || update
                    .add
                    .keys()
                    .chain(&update.remove)
                    .any(|name| !self.target.hosts.contains_key(name))
                || update
                    .remove
                    .iter()
                    .any(|name| update.add.contains_key(name))
                || update.remove.iter().collect::<BTreeSet<_>>().len() != update.remove.len()
            {
                return Err(invalid("corrupt campaign exclusion history"));
            }
        }
        let mut active = 0;
        for (index, attempt) in self.attempts.iter().enumerate() {
            if attempt.number != index as u64 + 1
                || attempt.target_digest != self.target_digest
                || attempt.generation > self.generation
                || attempt.targets.windows(2).any(|pair| pair[0] >= pair[1])
                || attempt
                    .targets
                    .iter()
                    .any(|name| !self.target.hosts.contains_key(name))
                || attempt
                    .run_id
                    .as_deref()
                    .is_some_and(|id| !registry::is_valid_run_id(id))
                || (attempt.state == AttemptState::Running && attempt.run_id.is_none())
            {
                return Err(invalid("corrupt campaign attempt reference"));
            }
            if attempt.kind != AttemptKind::Verify && attempt.state != AttemptState::Settled {
                active += 1;
            }
        }
        if active > 1 || (self.closure.is_some() && active != 0) {
            return Err(invalid("conflicting campaign attempt ownership"));
        }
        for (name, evidence) in &self.outcomes {
            let attempt = evidence
                .attempt
                .checked_sub(1)
                .and_then(|index| self.attempts.get(index as usize))
                .ok_or_else(|| invalid("outcome lacks attempt provenance"))?;
            if !attempt.targets.contains(name)
                || attempt.run_id.as_deref() != Some(evidence.run_id.as_str())
                || attempt.state != AttemptState::Settled
                || attempt.dry_activate
            {
                return Err(invalid("invalid campaign outcome provenance"));
            }
        }
        Ok(())
    }

    pub fn effective_exclusions(&self) -> BTreeMap<String, Exclusion> {
        let mut exclusions = self.target.initial_exclusions.clone();
        for update in &self.exclusion_updates {
            for name in &update.remove {
                // Capability notices cannot be removed, even by damaged history.
                if exclusions
                    .get(name)
                    .is_some_and(|e| e.kind == ExclusionKind::Operator)
                {
                    exclusions.remove(name);
                }
            }
            for (name, reason) in &update.add {
                if self.target.hosts.contains_key(name) {
                    exclusions.insert(
                        name.clone(),
                        Exclusion {
                            reason: reason.clone(),
                            actor: update.actor.clone(),
                            timestamp: update.timestamp,
                            kind: ExclusionKind::Operator,
                        },
                    );
                }
            }
        }
        exclusions
    }

    fn ensure_idle(&self, generation: u64) -> io::Result<()> {
        self.validate()?;
        if self.closure.is_some() {
            return Err(invalid("campaign is closed"));
        }
        if generation != self.generation {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "stale campaign generation",
            ));
        }
        if self.attempts.iter().any(|attempt| {
            attempt.kind != AttemptKind::Verify && attempt.state != AttemptState::Settled
        }) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "campaign mutation live or uncertain; reconcile ownership",
            ));
        }
        Ok(())
    }

    pub fn ensure_source(&self, observed: Option<&SourceIdentity>) -> io::Result<()> {
        if !observed.is_some_and(|source| self.target.source.same_content(source)) {
            return Err(invalid(
                "campaign source changed or unavailable; create a new campaign",
            ));
        }
        Ok(())
    }

    /// `narrow` is already resolved against bound membership, never latest inventory.
    pub fn eligible(
        &self,
        kind: AttemptKind,
        narrow: Option<&BTreeSet<String>>,
    ) -> io::Result<Vec<String>> {
        self.validate()?;
        if narrow.is_some_and(|names| {
            names
                .iter()
                .any(|name| !self.target.hosts.contains_key(name))
        }) {
            return Err(invalid("campaign selector cannot widen bound membership"));
        }
        let excluded = self.effective_exclusions();
        Ok(self
            .target
            .hosts
            .keys()
            .filter(|name| {
                if excluded.contains_key(*name)
                    || narrow.is_some_and(|names| !names.contains(*name))
                {
                    return false;
                }
                let outcome = self
                    .outcomes
                    .get(*name)
                    .map_or(HostOutcome::Pending, |e| e.outcome);
                match kind {
                    AttemptKind::Deploy => matches!(
                        outcome,
                        HostOutcome::Pending | HostOutcome::Failed | HostOutcome::RolledBack
                    ),
                    AttemptKind::Reboot => outcome == HostOutcome::RebootPending,
                    AttemptKind::Verify => true,
                }
            })
            .cloned()
            .collect())
    }

    /// Prepare ownership before allocating/linking a run. `None` means an empty
    /// eligible set, not a new run. Real mutations use the exact-name gate.
    pub fn prepare(&mut self, request: Dispatch<'_>) -> io::Result<Option<u64>> {
        self.ensure_idle(request.generation)?;
        if request.kind != AttemptKind::Verify {
            self.ensure_source(request.source)?;
        }
        let targets = self.eligible(request.kind, request.narrow)?;
        if targets.is_empty() {
            return Ok(None);
        }
        let required = targets.join(",");
        if (request.kind == AttemptKind::Reboot
            || (request.kind == AttemptKind::Deploy && !request.dry_activate))
            && request.confirm != Some(required.as_str())
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "required_confirm={required}; generation={}",
                    self.generation
                ),
            ));
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid("campaign generation overflow"))?;
        let number = self.attempts.len() as u64 + 1;
        self.attempts.push(Attempt {
            number,
            kind: request.kind,
            state: AttemptState::Prepared,
            generation: self.generation,
            target_digest: self.target_digest.clone(),
            targets,
            run_id: None,
            dry_activate: request.dry_activate,
        });
        Ok(Some(number))
    }

    pub fn link_run(&mut self, number: u64, run_id: &str) -> io::Result<()> {
        if !registry::is_valid_run_id(run_id) {
            return Err(invalid("invalid campaign run id"));
        }
        let attempt = self
            .attempts
            .iter_mut()
            .find(|a| a.number == number)
            .ok_or_else(|| invalid("unknown campaign attempt"))?;
        if attempt.state != AttemptState::Prepared || attempt.run_id.is_some() {
            return Err(invalid("campaign attempt already linked or reconciled"));
        }
        attempt.run_id = Some(run_id.to_string());
        attempt.state = AttemptState::Running;
        Ok(())
    }

    /// Only call with observed terminal per-host evidence, never merely a dead
    /// pid or a successful controller rc. Unproven ownership stays blocking.
    pub fn settle(
        &mut self,
        number: u64,
        evidence: BTreeMap<String, HostOutcome>,
    ) -> io::Result<()> {
        self.validate()?;
        let attempt = self
            .attempts
            .iter_mut()
            .find(|a| a.number == number)
            .ok_or_else(|| invalid("unknown campaign attempt"))?;
        if attempt.state == AttemptState::Settled {
            return Err(invalid("campaign attempt already settled"));
        }
        let run_id = attempt
            .run_id
            .clone()
            .ok_or_else(|| invalid("unlinked campaign attempt requires reconciliation"))?;
        if evidence.keys().cloned().collect::<Vec<_>>() != attempt.targets {
            return Err(invalid(
                "terminal evidence must cover exactly the dispatched members",
            ));
        }
        if !attempt.dry_activate && attempt.kind == AttemptKind::Deploy {
            for (host, outcome) in evidence {
                self.outcomes.insert(
                    host,
                    OutcomeEvidence {
                        outcome,
                        run_id: run_id.clone(),
                        attempt: number,
                    },
                );
            }
        }
        // Reboot and verification never grant activation success. Their fresh
        // survey evidence is recorded separately by the continuation adapter.
        attempt.state = AttemptState::Settled;
        Ok(())
    }

    pub fn update_exclusions(
        &mut self,
        generation: u64,
        mut update: ExclusionUpdate,
    ) -> io::Result<()> {
        self.ensure_idle(generation)?;
        if update.actor.trim().is_empty()
            || update.reason.trim().is_empty()
            || update.add.values().any(|reason| reason.trim().is_empty())
            || update
                .add
                .keys()
                .chain(&update.remove)
                .any(|name| !self.target.hosts.contains_key(name))
            || update
                .remove
                .iter()
                .any(|name| update.add.contains_key(name))
            || update.remove.iter().collect::<BTreeSet<_>>().len() != update.remove.len()
        {
            return Err(invalid(
                "invalid exclusion update; exact deploy members and reasons required",
            ));
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid("campaign generation overflow"))?;
        update.generation = self.generation;
        self.exclusion_updates.push(update);
        Ok(())
    }

    /// Administrative closure does not infer success, change targets or unroot
    /// anything. It remains possible when the original checkout is unavailable.
    pub fn close(&mut self, generation: u64, closure: Closure) -> io::Result<()> {
        self.ensure_idle(generation)?;
        if closure.actor.trim().is_empty() || closure.reason.trim().is_empty() {
            return Err(invalid("closing a campaign requires actor and reason"));
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid("campaign generation overflow"))?;
        self.closure = Some(closure);
        self.lifecycle = Lifecycle::Closed;
        Ok(())
    }
}

pub struct Dispatch<'a> {
    pub kind: AttemptKind,
    pub generation: u64,
    pub source: Option<&'a SourceIdentity>,
    pub narrow: Option<&'a BTreeSet<String>>,
    pub dry_activate: bool,
    pub confirm: Option<&'a str>,
}

/// A caller can identify a future schema without guessing its target format.
#[derive(Debug)]
pub enum Inspection {
    Supported(Box<Campaign>),
    Unsupported {
        campaign_id: String,
        schema_version: u64,
    },
}

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Default for Store {
    fn default() -> Self {
        Self::new(drift::state_dir().join("campaigns"))
    }
}

impl Store {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn path(&self, id: &str) -> io::Result<PathBuf> {
        if !is_valid_id(id) {
            return Err(invalid("invalid campaign id"));
        }
        reject_symlink(&self.root)?;
        let path = self.root.join(id);
        reject_symlink(&path)?;
        Ok(path)
    }

    pub fn inspect(&self, id: &str) -> io::Result<Inspection> {
        let path = self.path(id)?.join(MANIFEST);
        reject_symlink(&path)?;
        let bytes = std::fs::read(path)?;
        let value: Value = serde_json::from_slice(&bytes).map_err(invalid_json)?;
        let version = value["schema_version"]
            .as_u64()
            .ok_or_else(|| invalid("missing campaign schema version"))?;
        if value["campaign_id"].as_str() != Some(id) {
            return Err(invalid("campaign id mismatch"));
        }
        if version != SCHEMA_VERSION {
            return Ok(Inspection::Unsupported {
                campaign_id: id.into(),
                schema_version: version,
            });
        }
        let campaign: Campaign = serde_json::from_value(value).map_err(invalid_json)?;
        campaign.validate()?;
        Ok(Inspection::Supported(Box::new(campaign)))
    }

    pub fn load(&self, id: &str) -> io::Result<Campaign> {
        match self.inspect(id)? {
            Inspection::Supported(campaign) => Ok(*campaign),
            Inspection::Unsupported { schema_version, .. } => Err(invalid(format!(
                "unsupported campaign schema {schema_version}"
            ))),
        }
    }

    /// Lock files live outside campaign directories so retention cannot remove
    /// a locked inode then let a second writer lock a replacement inode.
    pub fn lock(&self, id: &str) -> io::Result<CampaignLock> {
        self.path(id)?;
        std::fs::create_dir_all(&self.root)?;
        let path = self.root.join(format!(".{id}.lock"));
        reject_symlink(&path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.try_lock().map_err(|error| {
            io::Error::new(io::ErrorKind::WouldBlock, format!("campaign busy: {error}"))
        })?;
        Ok(CampaignLock {
            file,
            root: self.root.clone(),
            id: id.into(),
        })
    }

    pub fn create(&self, target: Target, now: DateTime<Utc>) -> io::Result<Campaign> {
        target.validate()?;
        reject_symlink(&self.root)?;
        // Directory claim, not timestamp uniqueness, is the authority.
        std::fs::create_dir_all(&self.root)?;
        loop {
            let id = format!(
                "c-{}-{}",
                Utc::now().format("%Y%m%dT%H%M%S_%6f"),
                std::process::id()
            );
            let guard = self.lock(&id)?;
            let path = self.path(&id)?;
            match std::fs::create_dir(&path) {
                Ok(()) => {
                    let campaign = Campaign::new(id, target, now)?;
                    self.write(&guard, &campaign)?;
                    return Ok(campaign);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    /// Apply count/age policy to closed retention units only. Every referenced
    /// run is returned as protected from independent run pruning. Missing,
    /// corrupt and future manifests are never interpreted as permission to GC.
    pub(crate) fn retain(
        &self,
        runs: &Path,
        keep: usize,
        max_age_days: u64,
        now: DateTime<Utc>,
    ) -> BTreeSet<String> {
        let mut protected = BTreeSet::new();
        if reject_symlink(&self.root).is_err() {
            return protected;
        }
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return protected;
        };
        let mut closed = Vec::new();
        for entry in entries.flatten() {
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !is_valid_id(&id) {
                continue;
            }
            let Ok(path) = self.path(&id).map(|path| path.join(MANIFEST)) else {
                continue;
            };
            if reject_symlink(&path).is_err() {
                continue;
            }
            if let Ok(value) = std::fs::read(path)
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).map_err(invalid_json))
                && let Some(attempts) = value["attempts"].as_array()
            {
                for attempt in attempts {
                    if let Some(id) = attempt["run_id"]
                        .as_str()
                        .filter(|id| registry::is_valid_run_id(id))
                    {
                        protected.insert(id.to_string());
                    }
                }
            }
            if let Ok(campaign) = self.load(&id)
                && let Some(closure) = &campaign.closure
            {
                closed.push((closure.closed_at, id));
            }
        }
        closed.sort_by(|a, b| b.cmp(a));
        let mut survivors = 0;
        for (_, id) in closed {
            let Ok(_guard) = self.lock(&id) else { continue };
            let Ok(campaign) = self.load(&id) else {
                continue;
            };
            let Some(closure) = &campaign.closure else {
                continue;
            };
            let paths: Vec<_> = campaign
                .attempts
                .iter()
                .filter_map(|a| a.run_id.as_ref())
                .map(|id| runs.join(id))
                .collect();
            if paths.iter().any(|path| {
                let meta = registry::read_meta(path);
                // An absent/corrupt run cannot prove inactive ownership.
                meta.is_empty()
                    || registry::pid_alive(meta.get("pid").and_then(Value::as_i64))
                    || registry::pid_alive(meta.get("supervisor_pid").and_then(Value::as_i64))
            }) {
                continue;
            }
            survivors += 1;
            let expired = max_age_days > 0
                && now.signed_duration_since(closure.closed_at).num_days()
                    >= max_age_days.min(i64::MAX as u64) as i64;
            if survivors > keep || expired {
                let removed = paths
                    .iter()
                    .all(|path| match std::fs::remove_dir_all(path) {
                        Ok(()) => true,
                        Err(error) => error.kind() == io::ErrorKind::NotFound,
                    });
                if removed && let Ok(path) = self.path(&id) {
                    let _ = std::fs::remove_dir_all(path);
                }
            }
        }
        protected
    }

    pub fn transaction<T>(
        &self,
        id: &str,
        change: impl FnOnce(&mut Campaign) -> io::Result<T>,
    ) -> io::Result<T> {
        let guard = self.lock(id)?;
        let mut campaign = self.load(id)?;
        let original = campaign.clone();
        let result = change(&mut campaign)?;
        if campaign.target != original.target
            || campaign.target_digest != original.target_digest
            || campaign.campaign_id != original.campaign_id
            || campaign.created_at != original.created_at
            || campaign.schema_version != original.schema_version
            || campaign.generation < original.generation
            || !campaign
                .exclusion_updates
                .starts_with(&original.exclusion_updates)
            || !campaign.verification.starts_with(&original.verification)
            || (original.closure.is_some() && campaign != original)
            || campaign.attempts.len() < original.attempts.len()
            || original
                .attempts
                .iter()
                .zip(&campaign.attempts)
                .any(|(old, new)| {
                    old.number != new.number
                        || old.kind != new.kind
                        || old.generation != new.generation
                        || old.target_digest != new.target_digest
                        || old.targets != new.targets
                        || old.dry_activate != new.dry_activate
                        || (old.run_id.is_some() && old.run_id != new.run_id)
                        || (old.state == AttemptState::Settled && old != new)
                })
        {
            return Err(invalid("campaign immutable fields changed"));
        }
        self.write(&guard, &campaign)?;
        Ok(result)
    }

    fn write(&self, guard: &CampaignLock, campaign: &Campaign) -> io::Result<()> {
        if guard.root != self.root || guard.id != campaign.campaign_id {
            return Err(invalid("wrong campaign lock"));
        }
        campaign.validate()?;
        let path = self.path(&campaign.campaign_id)?;
        let tmp = path.join("campaign.json.tmp");
        reject_symlink(&tmp)?;
        let mut file = File::create(&tmp)?;
        file.write_all(&canonical(campaign)?)?;
        file.sync_all()?;
        std::fs::rename(tmp, path.join(MANIFEST))?;
        File::open(path)?.sync_all()
    }
}

fn reject_symlink(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(invalid("campaign paths must not be symlinks"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub struct CampaignLock {
    file: File,
    root: PathBuf,
    id: String,
}

impl Drop for CampaignLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests;
