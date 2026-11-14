//! Manifest inside a prebuilt owner profile (`mandala/vm-application.json`).
//! Store references make every guest artifact part of the owner's closure.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::OwnerUnit;

pub const VERSION: u64 = 1;
pub const RELATIVE_PATH: &str = "mandala/vm-application.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
pub struct Readiness {
    /// Host-local runtime/service probe contract. Guest SSH is not a probe.
    pub contract: String,
    pub deadline_seconds: u64,
}

/// A complete observed runtime, not desired config or an image symlink.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub manifest_version: u64,
    pub adapter: String,
    pub owner_profile: String,
    pub owner_running: String,
    pub boot_default: String,
    pub guests: BTreeMap<String, Runtime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

/// A field-addressable validation failure shared by runtime and authoring tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError {
    pub field: String,
    pub message: String,
}
impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}
impl std::error::Error for ManifestError {}
impl From<ManifestError> for String {
    fn from(error: ManifestError) -> Self {
        error.to_string()
    }
}
fn error(field: impl Into<String>, message: &str) -> ManifestError {
    ManifestError {
        field: field.into(),
        message: message.into(),
    }
}

impl Manifest {
    /// Parse the published manifest with field-addressable type errors.
    ///
    /// # Errors
    /// Missing/malformed fields or invalid JSON. Additive fields are ignored.
    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestError> {
        use serde_json::Value;
        let value: Value = serde_json::from_slice(bytes).map_err(|e| error("$", &e.to_string()))?;
        let string = |field: &str, value: &Value| {
            if value.is_string() {
                Ok(())
            } else {
                Err(error(field, "expected a string"))
            }
        };
        for field in [
            "owner",
            "backend",
            "ownerSystem",
            "ownerGeneration",
            "adapter",
        ] {
            string(field, &value[field])?;
        }
        if value["manifestVersion"].as_u64().is_none() {
            return Err(error("manifestVersion", "expected an unsigned integer"));
        }
        if !value["supportedSystems"]
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string))
        {
            return Err(error(
                "supportedSystems",
                "expected an array of system strings",
            ));
        }
        let guests = value["guests"]
            .as_object()
            .ok_or_else(|| error("guests", "expected a guest-ID keyed object"))?;
        for (name, guest) in guests {
            for field in [
                "system",
                "desiredIdentity",
                "artifact",
                "runtimeInstance",
                "recoveryContract",
            ] {
                string(&format!("guests.{name}.{field}"), &guest[field])?;
            }
            string(
                &format!("guests.{name}.readiness.contract"),
                &guest["readiness"]["contract"],
            )?;
            if guest["readiness"]["deadlineSeconds"].as_u64().is_none() {
                return Err(error(
                    format!("guests.{name}.readiness.deadlineSeconds"),
                    "expected an unsigned integer",
                ));
            }
        }
        serde_json::from_value(value).map_err(|e| error("$", &e.to_string()))
    }

    /// Validate exact compatibility and independently queried closure membership.
    ///
    /// # Errors
    /// The returned error names the first incompatible manifest field.
    pub fn validate(
        &self,
        unit: &OwnerUnit,
        closure: &BTreeSet<String>,
    ) -> Result<(), ManifestError> {
        if self.manifest_version != VERSION
            || self.manifest_version != unit.application.manifest_version
        {
            return Err(error(
                "manifestVersion",
                "unsupported or differs from host.vmApplication",
            ));
        }
        if self.owner != unit.owner {
            return Err(error("owner", "differs from approved owner identity"));
        }
        if self.owner_system != unit.owner_system {
            return Err(error("ownerSystem", "differs from owner build.system"));
        }
        if self.backend != "nixos-module" || self.backend != unit.application.backend {
            return Err(error(
                "backend",
                "unsupported or differs from host.vmApplication",
            ));
        }
        if self.guests.keys().ne(unit.affected_guests.iter()) {
            return Err(error(
                "guests",
                "must exactly match host.vmApplication and ownership relations",
            ));
        }
        if self.supported_systems != unit.application.supported_systems
            || !self.supported_systems.contains(&self.owner_system)
        {
            return Err(error(
                "supportedSystems",
                "must match host.vmApplication and support the owner system",
            ));
        }
        let dependency = |field: &str, path: &str| {
            let root = path.split('/').take(4).collect::<Vec<_>>().join("/");
            if store_reference(path) && !path.ends_with(".drv") && closure.contains(&root) {
                Ok(())
            } else {
                Err(error(
                    field,
                    "must be an immutable prebuilt reference in the owner application closure",
                ))
            }
        };
        dependency("ownerGeneration", &self.owner_generation)?;
        dependency("adapter", &self.adapter)?;
        for (name, guest) in &self.guests {
            let field = |key: &str| format!("guests.{name}.{key}");
            if guest.system != unit.application.guests[name].system
                || !self.supported_systems.contains(&guest.system)
            {
                return Err(error(
                    field("system"),
                    "differs from host.vmApplication or unsupported",
                ));
            }
            if !immutable_identity(&guest.desired_identity) {
                return Err(error(
                    field("desiredIdentity"),
                    "must be an immutable store path or lowercase sha256 digest",
                ));
            }
            if guest.runtime_instance.trim().is_empty() {
                return Err(error(
                    field("runtimeInstance"),
                    "must identify the owner-local runtime instance",
                ));
            }
            if !(1..=3600).contains(&guest.readiness.deadline_seconds) {
                return Err(error(
                    field("readiness.deadlineSeconds"),
                    "must be between 1 and 3600",
                ));
            }
            dependency(&field("artifact"), &guest.artifact)?;
            dependency(&field("readiness.contract"), &guest.readiness.contract)?;
            dependency(&field("recoveryContract"), &guest.recovery_contract)?;
        }
        Ok(())
    }

    /// Require complete prior owner/guest state, refusing fictitious rollback.
    ///
    /// # Errors
    /// The error names missing/unrecoverable previous evidence.
    pub fn validate_previous(&self, previous: &Observation) -> Result<(), ManifestError> {
        if previous.manifest_version != VERSION {
            return Err(error("previous.manifestVersion", "unsupported"));
        }
        for (field, path) in [
            ("adapter", &previous.adapter),
            ("ownerProfile", &previous.owner_profile),
            ("ownerRunning", &previous.owner_running),
            ("bootDefault", &previous.boot_default),
        ] {
            if !store_reference(path) {
                return Err(error(
                    format!("previous.{field}"),
                    "missing immutable previous reference",
                ));
            }
        }
        if previous.guests.keys().ne(self.guests.keys()) {
            return Err(error(
                "previous.guests",
                "every affected guest requires a prior runtime; first-time creation refused",
            ));
        }
        for (name, runtime) in &previous.guests {
            let field = |key: &str| format!("previous.guests.{name}.{key}");
            if !immutable_identity(&runtime.running_identity) {
                return Err(error(
                    field("runningIdentity"),
                    "missing recoverable running identity; first-time creation refused",
                ));
            }
            if !runtime.ready {
                return Err(error(field("ready"), "prior runtime must be ready"));
            }
            if runtime.runtime_instance != self.guests[name].runtime_instance {
                return Err(error(
                    field("runtimeInstance"),
                    "does not match the affected runtime",
                ));
            }
            for (key, reference) in [
                ("artifact", &runtime.artifact),
                ("readiness.contract", &runtime.readiness.contract),
                ("recoveryContract", &runtime.recovery_contract),
            ] {
                if !store_reference(reference) {
                    return Err(error(field(key), "missing immutable recovery reference"));
                }
            }
            if !(1..=3600).contains(&runtime.readiness.deadline_seconds) {
                return Err(error(
                    field("readiness.deadlineSeconds"),
                    "must be between 1 and 3600",
                ));
            }
        }
        Ok(())
    }
}
