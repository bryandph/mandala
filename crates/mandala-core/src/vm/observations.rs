//! Read-only reconstruction and shared settlement. This module never evaluates
//! a fleet, queries a store, opens a transport, or invents missing VM evidence.

use super::owner::{GuestOutcome, Journal, State, VERSION};
use super::{OwnerPlan, OwnerUnit};

#[cfg(test)]
#[path = "observations_tests.rs"]
mod tests;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Verified,
    Pending,
    Preview,
    RolledBack,
    Failed,
    PartialRecovery,
    Unknown,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerView {
    pub verdict: Verdict,
    pub guests: BTreeMap<String, GuestOutcome>,
    pub diagnostic: Option<String>,
    pub journal: Option<Journal>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub verified: usize,
    pub pending: usize,
    pub preview: usize,
    pub restored: usize,
    pub failed: usize,
    pub unknown: usize,
}
impl Summary {
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "VM guests: verified={} pending={} preview={} restored={} failed={} unknown={}",
            self.verified, self.pending, self.preview, self.restored, self.failed, self.unknown
        )
    }
}
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VmRun {
    pub plan: Option<OwnerPlan>,
    pub approval: Option<serde_json::Value>,
    pub owners: BTreeMap<String, OwnerView>,
    pub summary: Summary,
    pub error: Option<String>,
}
impl VmRun {
    /// Load only durable run files; absent VM metadata preserves old records.
    #[must_use]
    pub fn read(directory: &Path) -> Self {
        let meta = crate::registry::read_meta(directory);
        let Some(raw) = meta.get("vm_owner_plan") else {
            return if directory.join("vm").is_dir() {
                Self {
                    error: Some("VM observations have no approved owner plan".into()),
                    ..Self::default()
                }
            } else {
                Self::default()
            };
        };
        let plan: OwnerPlan = match serde_json::from_value(raw.clone()) {
            Ok(plan) => plan,
            Err(error) => {
                return Self {
                    error: Some(format!("unreadable approved VM plan: {error}")),
                    ..Self::default()
                };
            }
        };
        let context = meta.get("vm_owner_context");
        let revision = meta
            .get("vm_inventory_identity")
            .and_then(serde_json::Value::as_str);
        let fingerprint = meta
            .get("vm_owner_approval")
            .and_then(serde_json::Value::as_str);
        let valid_approval = plan.validate_bound().is_ok()
            && context.is_some_and(|c| {
                c["settings"].is_object()
                    && c["targets"].as_array().is_some_and(|targets| {
                        plan.units
                            .keys()
                            .all(|owner| targets.contains(&serde_json::Value::from(owner.clone())))
                            && plan
                                .units
                                .values()
                                .flat_map(|unit| unit.affected_guests.iter())
                                .all(|guest| {
                                    !targets.contains(&serde_json::Value::from(guest.clone()))
                                })
                    })
                    && c.get("targets") == meta.get("targets")
                    && c["boot"].as_bool()
                        == Some(
                            meta.get("boot")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false),
                        )
                    && c["dryActivate"].as_bool()
                        == Some(
                            meta.get("dry_activate")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false),
                        )
            })
            && revision.is_some_and(|r| {
                !r.is_empty()
                    && context
                        .is_some_and(|c| plan.fingerprint(r, c).ok().as_deref() == fingerprint)
            });
        if !valid_approval {
            return Self {
                summary: Summary {
                    unknown: plan.units.values().map(|u| u.affected_guests.len()).sum(),
                    ..Summary::default()
                },
                plan: Some(plan),
                error: Some("missing, stale or inconsistent approved VM owner context".into()),
                ..Self::default()
            };
        }
        let dry = meta
            .get("dry_activate")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let mut run = Self {
            plan: Some(plan.clone()),
            approval: Some(
                serde_json::json!({"fingerprint":fingerprint,"revision":revision,"context":context}),
            ),
            ..Self::default()
        };
        for (owner, unit) in &plan.units {
            if !crate::inventory::is_valid_member_name(owner) || unit.owner != *owner {
                run.error = Some("invalid owner identity in approved plan".into());
                continue;
            }
            let path = directory.join("vm").join(format!("{owner}.json"));
            let view = match std::fs::read(path)
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    serde_json::from_slice::<Journal>(&bytes).map_err(|e| e.to_string())
                }) {
                Ok(journal) => judge(unit, journal, dry),
                Err(error) => OwnerView {
                    verdict: Verdict::Unknown,
                    guests: unit
                        .affected_guests
                        .iter()
                        .map(|g| (g.clone(), GuestOutcome::Unknown))
                        .collect(),
                    diagnostic: Some(format!("missing/unreadable runtime evidence: {error}")),
                    journal: None,
                },
            };
            for outcome in view.guests.values() {
                match outcome {
                    GuestOutcome::Confirmed => run.summary.verified += 1,
                    GuestOutcome::Pending => run.summary.pending += 1,
                    GuestOutcome::Preview => run.summary.preview += 1,
                    GuestOutcome::Restored => run.summary.restored += 1,
                    GuestOutcome::Failed => run.summary.failed += 1,
                    GuestOutcome::Unknown => run.summary.unknown += 1,
                }
            }
            run.owners.insert(owner.clone(), view);
        }
        run
    }
    #[must_use]
    pub fn present(&self) -> bool {
        self.plan.is_some() || self.error.is_some()
    }
    #[must_use]
    pub fn has_failure(&self) -> bool {
        self.error.is_some()
            || self.owners.values().any(|o| {
                matches!(
                    o.verdict,
                    Verdict::Failed | Verdict::PartialRecovery | Verdict::Unknown
                )
            })
    }
    #[must_use]
    pub fn has_rollback(&self) -> bool {
        self.owners
            .values()
            .any(|o| o.verdict == Verdict::RolledBack)
    }
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.error
            .iter()
            .map(|error| format!("VM evidence unavailable: {error}"))
            .chain(self.owners.keys().flat_map(|owner| self.owner_lines(owner)))
            .collect()
    }
    #[must_use]
    pub fn owner_lines(&self, owner: &str) -> Vec<String> {
        let Some(view) = self.owners.get(owner) else {
            return Vec::new();
        };
        let mut lines = vec![format!("VM owner {owner}: {:?}", view.verdict)];
        for (guest, outcome) in &view.guests {
            let observed = view
                .journal
                .as_ref()
                .and_then(|j| j.observed.as_ref())
                .and_then(|o| o.guests.get(guest));
            lines.push(format!(
                "VM guest {guest} owner {owner}: {outcome:?}; observed={}; ready={}",
                observed
                    .map(|g| g.running_identity.as_str())
                    .unwrap_or("unknown"),
                observed.is_some_and(|g| g.ready)
            ));
        }
        if let Some(diagnostic) = &view.diagnostic {
            lines.push(diagnostic.clone());
        }
        if let Some(journal) = &view.journal {
            if let Some(primary) = &journal.primary_failure {
                lines.push(format!("VM primary failure: {primary}"));
            }
            if let Some(recovery) = &journal.recovery_failure {
                lines.push(format!("VM recovery failure: {recovery}"));
            }
            lines.push(journal.recovery_limit.clone());
        }
        lines
    }
    #[must_use]
    pub fn unsuccessful(&self) -> bool {
        self.has_failure() || self.has_rollback()
    }
}

/// Recompute success from observed evidence, never a terminal label or rc alone.
#[must_use]
pub fn judge(unit: &OwnerUnit, journal: Journal, dry: bool) -> OwnerView {
    let mut view = OwnerView {
        verdict: Verdict::Unknown,
        guests: unit
            .affected_guests
            .iter()
            .map(|g| (g.clone(), GuestOutcome::Unknown))
            .collect(),
        diagnostic: None,
        journal: Some(journal.clone()),
    };
    let validate = || -> Result<(), String> {
        if journal.version != VERSION {
            return Err(format!(
                "unsupported VM evidence version {} (reader {VERSION})",
                journal.version
            ));
        }
        if journal.owner != unit.owner || journal.unit.as_ref() != Some(unit) {
            return Err("runtime record differs from approved owner plan".into());
        }
        let manifest = journal
            .manifest
            .as_ref()
            .ok_or("missing runtime manifest")?;
        if !journal
            .profile
            .as_deref()
            .is_some_and(super::manifest::store_reference)
        {
            return Err("missing immutable desired owner profile".into());
        }
        // Closure membership was qualified before mutation; readers only check
        // logical/immutable manifest consistency, without contacting the store.
        let mut references = BTreeSet::from([
            manifest.owner_generation.clone(),
            manifest
                .adapter
                .split('/')
                .take(4)
                .collect::<Vec<_>>()
                .join("/"),
        ]);
        for guest in manifest.guests.values() {
            references.extend([
                guest.artifact.clone(),
                guest
                    .readiness
                    .contract
                    .split('/')
                    .take(4)
                    .collect::<Vec<_>>()
                    .join("/"),
                guest
                    .recovery_contract
                    .split('/')
                    .take(4)
                    .collect::<Vec<_>>()
                    .join("/"),
            ]);
        }
        manifest.validate(unit, &references).map_err(String::from)?;
        manifest
            .validate_previous(
                journal
                    .previous
                    .as_ref()
                    .ok_or("missing previous runtime evidence")?,
            )
            .map_err(String::from)?;
        if journal.guests.keys().ne(unit.affected_guests.iter()) {
            return Err("missing affected guest dispositions".into());
        }
        Ok(())
    };
    if let Err(error) = validate() {
        view.diagnostic = Some(error);
        return view;
    }
    view.guests = journal.guests.clone();
    match journal.state {
        State::Confirmed | State::Pending | State::Restored => {
            let Some(observed) = journal.observed.as_ref() else {
                view.diagnostic = Some("terminal label without running observations".into());
                view.guests
                    .values_mut()
                    .for_each(|g| *g = GuestOutcome::Unknown);
                return view;
            };
            let manifest = journal.manifest.as_ref().unwrap();
            let previous = journal.previous.as_ref().unwrap();
            let expected_outcome = match journal.state {
                State::Confirmed => GuestOutcome::Confirmed,
                State::Pending => GuestOutcome::Pending,
                _ => GuestOutcome::Restored,
            };
            let mut good = observed.guests.keys().eq(manifest.guests.keys());
            for (name, desired) in &manifest.guests {
                let expected = &previous.guests[name];
                let matches = observed.guests.get(name).is_some_and(|runtime| {
                    runtime.ready
                        && runtime.runtime_instance == desired.runtime_instance
                        && if journal.state == State::Confirmed {
                            runtime.running_identity == desired.desired_identity
                                && runtime.artifact == desired.artifact
                                && runtime.readiness == desired.readiness
                                && runtime.recovery_contract == desired.recovery_contract
                        } else {
                            runtime.running_identity == expected.running_identity
                                && runtime.artifact == expected.artifact
                                && runtime.readiness == expected.readiness
                                && runtime.recovery_contract == expected.recovery_contract
                        }
                });
                let matches = matches && journal.guests[name] == expected_outcome;
                good &= matches;
                if !matches {
                    view.guests.insert(name.clone(), GuestOutcome::Failed);
                }
            }
            good &= match journal.state {
                State::Confirmed => {
                    journal.profile.as_ref() == Some(&observed.owner_profile)
                        && observed.owner_running == manifest.owner_generation
                }
                State::Pending => {
                    journal.profile.as_ref() == Some(&observed.owner_profile)
                        && observed.boot_default == manifest.owner_generation
                        && observed.owner_running == previous.owner_running
                }
                _ => {
                    observed.owner_profile == previous.owner_profile
                        && observed.owner_running == previous.owner_running
                        && observed.boot_default == previous.boot_default
                }
            };
            view.verdict = if good {
                match journal.state {
                    State::Confirmed => Verdict::Verified,
                    State::Pending => Verdict::Pending,
                    _ => Verdict::RolledBack,
                }
            } else if journal.state == State::Restored {
                Verdict::PartialRecovery
            } else {
                Verdict::Failed
            };
            if !good {
                view.diagnostic = Some(
                    "owner/guest running identity or readiness does not prove claimed settlement"
                        .into(),
                );
            }
        }
        State::Prepared if dry && journal.guests.values().all(|g| *g == GuestOutcome::Preview) => {
            view.verdict = Verdict::Preview
        }
        State::PartialRecovery => view.verdict = Verdict::PartialRecovery,
        State::FailedUnrecovered | State::NotApplied => view.verdict = Verdict::Failed,
        _ => {
            view.verdict = Verdict::Unknown;
            view.guests
                .values_mut()
                .for_each(|g| *g = GuestOutcome::Unknown);
        }
    }
    view
}
