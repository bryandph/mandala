//! Pure owner-unit plans and the versioned consumer runtime contract.
//!
//! Member identity is never a transport endpoint. Runtime operations consume
//! one owner artifact; guest paths are dependencies of that artifact.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::inventory::{Inventory, InventoryError};

pub mod manifest;
pub mod observations;
pub mod owner;

/// Campaign mutation stays closed until frozen owner units and retry evidence
/// have been integrated. Non-VM campaigns retain their existing behavior.
///
/// # Errors
/// VM-bearing selection or an invalid/conflicting selection.
pub fn ensure_campaign_supported(inv: &Inventory, selector: &str) -> Result<(), InventoryError> {
    if !resolve(inv, selector)?.units.is_empty() {
        return Err(invalid(
            "VM owner units are not yet supported by frozen campaign targets",
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;

/// The first backend deliberately does not promise writable-data recovery.
pub const RECOVERY_LIMIT: &str = "Runtime/image restoration only; writable disks, application data and external services are not reverted.";

/// Owner-side compatibility summary published by the consumer module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Application {
    pub owner: String,
    pub backend: String,
    pub manifest_version: u64,
    pub supported_systems: Vec<String>,
    pub guests: BTreeMap<String, GuestSystem>,
    pub application: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestSystem {
    pub system: String,
}

/// A conservative application boundary, including unrequested siblings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerUnit {
    pub owner: String,
    pub owner_system: String,
    pub requested_guests: Vec<String>,
    pub affected_guests: Vec<String>,
    pub potentially_affected_guests: Vec<String>,
    pub application: Application,
    pub recovery_limit: String,
}

/// Shared immutable frontend preview. Approval must bind the complete value,
/// not merely the selector. The fingerprint includes settings and revision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerPlan {
    pub selected_members: Vec<String>,
    pub requested_guests: Vec<String>,
    pub units: BTreeMap<String, OwnerUnit>,
}

impl OwnerPlan {
    /// Deterministic approval identity; no transport or evaluation is performed.
    ///
    /// # Errors
    /// Returns serialization errors for unsupported caller-provided settings.
    pub fn fingerprint(
        &self,
        revision: &str,
        settings: &Value,
    ) -> Result<String, serde_json::Error> {
        let bytes = serde_json::to_vec(&(revision, self, settings))?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

fn invalid(message: impl Into<String>) -> InventoryError {
    InventoryError::Malformed(message.into())
}

fn enabled(member: &crate::inventory::Member, surface: &str) -> bool {
    member
        .get("deployment")
        .and_then(|v| v.get(surface))
        .and_then(|v| v.get("enable"))
        .and_then(Value::as_bool)
        == Some(true)
}

/// Validate the authoritative relation independently of consumer projections.
/// A forged version-2 aggregate cannot turn a guest into a transport target.
///
/// # Errors
/// Rejects unsupported pairs, inconsistent indices/application summaries and
/// contradictory transport enables before plan registration or build.
pub fn validate(inv: &Inventory) -> Result<(), InventoryError> {
    let mut index: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, member) in inv.members() {
        let kind = member
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("standalone");
        let relation = member.get("vm").filter(|v| !v.is_null());
        if kind == "standalone" && relation.is_none() {
            continue;
        }
        if kind != "vm" || relation.is_none() {
            return Err(invalid(format!(
                "{name}: contradictory execution kind/VM relation"
            )));
        }
        if inv.aggregate().schema_version != 2 {
            return Err(invalid("VM relations require aggregate schemaVersion 2"));
        }
        let relation = relation.expect("checked relation");
        let owner = relation["owner"]
            .as_str()
            .ok_or_else(|| invalid(format!("{name}: missing VM owner")))?;
        let parent = inv
            .members()
            .get(owner)
            .ok_or_else(|| invalid(format!("{name}: missing owner {owner}")))?;
        if owner == name
            || parent
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("standalone")
                != "standalone"
            || parent.get("vm").is_some_and(|v| !v.is_null())
            || parent.get("platform").and_then(Value::as_str) != Some("nixos")
            || member.get("platform").and_then(Value::as_str) != Some("nixos")
            || !enabled(parent, "deployRs")
            || relation["backend"] != "nixos-module"
            || enabled(member, "deployRs")
            || enabled(member, "ansible")
            || member
                .get("builder")
                .and_then(|v| v.get("enable"))
                .and_then(Value::as_bool)
                == Some(true)
        {
            return Err(invalid(format!(
                "{name}: unsupported VM owner/backend or guest transport declaration ({owner})"
            )));
        }
        index.entry(owner.into()).or_default().push(name.clone());
    }
    let projections = inv.aggregate().extra.get("projections");
    let vm = projections.and_then(|v| v.get("vm"));
    if index.is_empty() {
        if vm.is_some_and(|v| v["applications"].as_object().is_some_and(|a| !a.is_empty())) {
            return Err(invalid("VM application has no guest relations"));
        }
        return Ok(());
    }
    let vm = vm.ok_or_else(|| invalid("missing VM projection"))?;
    let projected: BTreeMap<String, Vec<String>> =
        serde_json::from_value(vm["guestsByOwner"].clone())
            .map_err(|e| invalid(format!("invalid VM index: {e}")))?;
    if projected != index {
        return Err(invalid("VM index disagrees with member ownership"));
    }
    let applications: BTreeMap<String, Application> =
        serde_json::from_value(vm["applications"].clone())
            .map_err(|e| invalid(format!("invalid VM applications: {e}")))?;
    if applications.keys().ne(index.keys()) {
        return Err(invalid("VM applications disagree with owner index"));
    }
    for (owner, guests) in &index {
        let app = &applications[owner];
        if app.owner != *owner
            || app.backend != "nixos-module"
            || app.manifest_version != 1
            || app.application != format!(".#deploy.nodes.{owner}.profiles.system.path")
            || app.guests.keys().ne(guests.iter())
        {
            return Err(invalid(format!(
                "{owner}: unsupported or inconsistent application summary"
            )));
        }
        for name in std::iter::once(owner).chain(guests) {
            let system = inv.members()[name]
                .get("build")
                .and_then(|v| v.get("system"))
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(format!("{name}: missing build system")))?;
            if !app.supported_systems.iter().any(|s| s == system)
                || (name != owner && app.guests[name].system != system)
            {
                return Err(invalid(format!(
                    "{owner}: unsupported or inconsistent system for {name}"
                )));
            }
        }
    }
    Ok(())
}

/// Resolve once, retaining explicit exclusions so sibling impact cannot silently
/// defeat an operator exclusion. Non-VM selections yield an empty owner plan.
///
/// # Errors
/// Invalid selectors/ownership or an excluded owner/affected sibling.
pub fn resolve(inv: &Inventory, selector: &str) -> Result<OwnerPlan, InventoryError> {
    let (selected, excluded) = inv.resolve_with_exclusions(selector)?;
    resolve_selected(inv, selected, excluded)
}

pub(crate) fn resolve_selected(
    inv: &Inventory,
    selected: Vec<String>,
    excluded: Vec<String>,
) -> Result<OwnerPlan, InventoryError> {
    validate(inv)?;
    let mut requested = Vec::new();
    let mut owners = BTreeSet::new();
    let vm = inv
        .aggregate()
        .extra
        .get("projections")
        .and_then(|v| v.get("vm"));
    for name in &selected {
        let member = &inv.members()[name];
        if member.get("kind").and_then(Value::as_str) == Some("vm") {
            requested.push(name.clone());
            owners.insert(
                member
                    .get("vm")
                    .and_then(|v| v["owner"].as_str())
                    .expect("validated relation")
                    .to_string(),
            );
        } else if vm.is_some_and(|v| !v["applications"][name].is_null()) {
            owners.insert(name.clone());
        }
    }
    if owners.is_empty() {
        return Ok(OwnerPlan::default());
    }
    let vm = vm.expect("validated projection");
    let mut units = BTreeMap::new();
    for owner in owners {
        let application: Application = serde_json::from_value(vm["applications"][&owner].clone())
            .map_err(|e| invalid(e.to_string()))?;
        let affected_guests: Vec<_> = application.guests.keys().cloned().collect();
        if std::iter::once(&owner)
            .chain(&affected_guests)
            .any(|n| excluded.contains(n))
        {
            return Err(invalid(format!(
                "owner application {owner} conflicts with explicit exclusions"
            )));
        }
        let requested_guests: Vec<_> = affected_guests
            .iter()
            .filter(|n| requested.contains(n))
            .cloned()
            .collect();
        let potentially_affected_guests = affected_guests
            .iter()
            .filter(|n| !requested.contains(n))
            .cloned()
            .collect();
        units.insert(
            owner.clone(),
            OwnerUnit {
                owner_system: inv.members()[&owner]
                    .get("build")
                    .and_then(|v| v["system"].as_str())
                    .expect("validated owner system")
                    .into(),
                owner,
                requested_guests,
                affected_guests,
                potentially_affected_guests,
                application,
                recovery_limit: RECOVERY_LIMIT.into(),
            },
        );
    }
    Ok(OwnerPlan {
        selected_members: selected,
        requested_guests: requested,
        units,
    })
}
