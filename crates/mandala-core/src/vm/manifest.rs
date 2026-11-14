//! Manifest inside a prebuilt owner profile (`mandala/vm-application.json`).
//! Store references make every guest artifact part of the owner's closure.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::OwnerUnit;

pub const VERSION: u64 = 1;
pub const RELATIVE_PATH: &str = "mandala/vm-application.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub manifest_version: u64,
    pub owner: String,
    pub backend: String,
    pub owner_system: String,
    /// The prebuilt owner toplevel (deploy-rs may wrap it in a profile).
    pub owner_generation: String,
    pub supported_systems: Vec<String>,
    /// Consumer-supplied typed runtime protocol executable, not a shell hook.
    pub adapter: String,
    pub guests: BTreeMap<String, Guest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Guest {
    pub system: String,
    /// Immutable running generation/image identity required from inspection.
    pub desired_identity: String,
    /// Direct immutable store references to closure/image and contracts.
    pub artifact: String,
    pub runtime_instance: String,
    pub readiness: Readiness,
    pub recovery_contract: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Readiness {
    /// Host-local runtime/service probe contract. Guest SSH is not a probe.
    pub contract: String,
    pub deadline_seconds: u64,
}

/// A complete observed runtime, not desired config or an image symlink.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Observation {
    pub manifest_version: u64,
    pub adapter: String,
    pub owner_profile: String,
    pub owner_running: String,
    pub boot_default: String,
    pub guests: BTreeMap<String, Runtime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Runtime {
    pub running_identity: String,
    pub runtime_instance: String,
    pub artifact: String,
    pub readiness: Readiness,
    pub recovery_contract: String,
    pub ready: bool,
    pub diagnostic: String,
}

pub(crate) fn store_reference(path: &str) -> bool {
    let Some(tail) = path.strip_prefix("/nix/store/") else {
        return false;
    };
    let mut components = tail.split('/');
    let Some(root) = components.next() else {
        return false;
    };
    let Some((hash, name)) = root.split_once('-') else {
        return false;
    };
    hash.len() == 32
        && hash
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+-._?=".contains(&b))
        && components.all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"+-._".contains(&b))
        })
}

pub(crate) fn immutable_identity(identity: &str) -> bool {
    (store_reference(identity) && !identity.ends_with(".drv"))
        || identity.strip_prefix("sha256:").is_some_and(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        })
}

impl Manifest {
    /// Validate the built artifact against the preview and its actual closure.
    /// The closure must come from a store query, not from this manifest.
    ///
    /// # Errors
    /// Rejects unsupported versions/systems, missing dependency references,
    /// unbounded readiness or mismatched owner/guest impact.
    pub fn validate(&self, unit: &OwnerUnit, closure: &BTreeSet<String>) -> Result<(), String> {
        if self.manifest_version != VERSION
            || self.manifest_version != unit.application.manifest_version
            || self.owner != unit.owner
            || self.owner_system != unit.owner_system
            || self.backend != "nixos-module"
            || self.backend != unit.application.backend
            || self.guests.keys().ne(unit.affected_guests.iter())
            || self.supported_systems != unit.application.supported_systems
            || !self.supported_systems.contains(&self.owner_system)
        {
            return Err("owner manifest does not match approved application".into());
        }
        let dependency = |path: &str| {
            if !store_reference(path) {
                return false;
            }
            let root = path.split('/').take(4).collect::<Vec<_>>().join("/");
            closure.contains(&root)
        };
        if !dependency(&self.owner_generation) || !dependency(&self.adapter) {
            return Err("owner adapter is not an immutable application dependency".into());
        }
        for (name, guest) in &self.guests {
            if guest.system != unit.application.guests[name].system
                || !self.supported_systems.contains(&guest.system)
                || !immutable_identity(&guest.desired_identity)
                || guest.runtime_instance.trim().is_empty()
                || !(1..=3600).contains(&guest.readiness.deadline_seconds)
                || !dependency(&guest.artifact)
                || !dependency(&guest.readiness.contract)
                || !dependency(&guest.recovery_contract)
            {
                return Err(format!(
                    "{name}: incomplete/unbounded runtime manifest or missing closure dependency"
                ));
            }
        }
        Ok(())
    }

    /// Require prior owner state and every previous running sibling. The first
    /// backend cannot claim to recover initial creation or missing evidence.
    ///
    /// # Errors
    /// Missing/ambiguous runtime, instance mismatch, unreadiness or no artifact.
    pub fn validate_previous(&self, previous: &Observation) -> Result<(), String> {
        if previous.manifest_version != VERSION
            || !store_reference(&previous.adapter)
            || !store_reference(&previous.owner_profile)
            || !store_reference(&previous.owner_running)
            || !store_reference(&previous.boot_default)
            || previous.guests.keys().ne(self.guests.keys())
        {
            return Err("previous owner/guest state is not recoverable".into());
        }
        for (name, runtime) in &previous.guests {
            if !immutable_identity(&runtime.running_identity)
                || !runtime.ready
                || runtime.runtime_instance != self.guests[name].runtime_instance
                || !store_reference(&runtime.artifact)
                || !store_reference(&runtime.readiness.contract)
                || !(1..=3600).contains(&runtime.readiness.deadline_seconds)
                || !store_reference(&runtime.recovery_contract)
            {
                return Err(format!(
                    "{name}: no verified recoverable prior runtime; first-time creation refused"
                ));
            }
        }
        Ok(())
    }
}
