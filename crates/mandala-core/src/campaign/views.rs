//! Shared read projections. Target comparison never consults a latest-source
//! cache, and latest-source comparison never substitutes for a missing target.

use super::*;
use crate::drift::{DriftEntry, DriftStatus, Snapshot, Toplevels};
use serde_json::json;

pub fn source_summary(source: &SourceIdentity) -> io::Result<Value> {
    let inputs: BTreeMap<_, _> = source
        .inputs
        .iter()
        .map(|(name, node)| (name, &node.content_digest))
        .collect();
    let modules: BTreeMap<_, _> = source
        .submodules
        .iter()
        .map(|(name, module)| {
            (
                name,
                json!({
                    "gitlink_revision":module.gitlink_revision,
                    "checkout_revision":module.checkout_revision,
                    "content_digest":module.content.content_digest,
                }),
            )
        })
        .collect();
    let content = json!({"root":source.root,"graph_digest":source.graph_digest,"lock_digest":source.lock_digest,"inputs":inputs,"submodules":modules});
    Ok(json!({
        "checkout":source.checkout, "revision":source.revision, "dirty":source.dirty,
        "root":source.root,"snapshot":source.snapshot, "graph_digest":source.graph_digest,"lock_digest":source.lock_digest,
        "content_identity":digest(&canonical(&content)?),
        "inputs_total":source.inputs.len(), "submodules_total":source.submodules.len(),
    }))
}

fn next(offset: usize, count: usize, total: usize) -> Option<usize> {
    (offset.saturating_add(count) < total).then_some(offset.saturating_add(count))
}

pub fn inspect(store: &Store, id: Option<&str>, offset: usize, limit: usize) -> io::Result<Value> {
    reject_symlink(&store.root)?;
    let limit = limit.clamp(1, 100);
    let Some(id) = id else {
        let mut ids: Vec<_> = match std::fs::read_dir(&store.root) {
            Ok(entries) => entries
                .flatten()
                .filter_map(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .filter(|id| is_valid_id(id))
                        .map(str::to_string)
                })
                .collect(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        ids.sort();
        ids.reverse();
        let total = ids.len();
        let entries:Vec<_>=ids.into_iter().skip(offset).take(limit).map(|id|match store.load(&id){
            Ok(c)=>json!({"campaign_id":id,"lifecycle":c.lifecycle,"generation":c.generation,"created_at":c.created_at,"created_by":c.created_by,"target_digest":c.target_digest}),
            Err(error)=>json!({"campaign_id":id,"unavailable":true,"error":error.to_string()}),
        }).collect();
        return Ok(
            json!({"ok":true,"campaigns":entries,"total":total,"next_offset":next(offset,entries.len(),total)}),
        );
    };
    let c = match store.inspect(id)? {
        Inspection::Unsupported {
            campaign_id,
            schema_version,
        } => {
            return Ok(
                json!({"ok":false,"campaign_id":campaign_id,"schema_version":schema_version,"unsupported":true}),
            );
        }
        Inspection::Supported(c) => c,
    };
    let excluded = c.effective_exclusions();
    let members = &c.target.selection.members;
    let rows:Vec<_>=members.iter().skip(offset).take(limit).map(|name|json!({
        "name":name,"target":c.target.hosts.get(name),"exclusion":excluded.get(name),"outcome":c.outcomes.get(name),
    })).collect();
    let attempts: Vec<_> = c.attempts.iter().rev().skip(offset).take(limit).collect();
    let inputs: BTreeMap<_, _> = c
        .target
        .source
        .inputs
        .iter()
        .skip(offset)
        .take(limit)
        .collect();
    let modules: BTreeMap<_, _> = c
        .target
        .source
        .submodules
        .iter()
        .skip(offset)
        .take(limit)
        .collect();
    let mut source = source_summary(&c.target.source)?;
    source["inputs"] = serde_json::to_value(&inputs).map_err(invalid_json)?;
    source["submodules"] = serde_json::to_value(&modules).map_err(invalid_json)?;
    source["inputs_next_offset"] = json!(next(offset, inputs.len(), c.target.source.inputs.len()));
    source["submodules_next_offset"] = json!(next(
        offset,
        modules.len(),
        c.target.source.submodules.len()
    ));
    let verification = if let Some(verification) = c.verification.last() {
        let mut value = serde_json::to_value(verification).map_err(invalid_json)?;
        let results: BTreeMap<_, _> = verification
            .results
            .iter()
            .skip(offset)
            .take(limit)
            .collect();
        value["results"] = serde_json::to_value(&results).map_err(invalid_json)?;
        value["results_total"] = json!(verification.results.len());
        value["results_next_offset"] =
            json!(next(offset, results.len(), verification.results.len()));
        value
    } else {
        Value::Null
    };
    Ok(json!({
        "ok":true,"campaign_id":c.campaign_id,"schema_version":c.schema_version,
        "created_at":c.created_at,"created_by":c.created_by,"lifecycle":c.lifecycle,
        "closure":c.closure,"generation":c.generation,"target_digest":c.target_digest,
        "source":source,"source_state":"unchecked","members":rows,
        "original_count":members.len(),
        "eligible_count":c.target.hosts.len()-excluded.keys().filter(|n|c.target.hosts.contains_key(*n)).count(),
        "excluded_count":excluded.len(),"next_offset":next(offset,rows.len(),members.len()),
        "attempts":attempts,"attempts_total":c.attempts.len(),
        "attempts_elided":c.attempts.len().saturating_sub(attempts.len()),
        "attempts_next_offset":next(offset,attempts.len(),c.attempts.len()),
        "verification":verification,"verification_total":c.verification.len(),
    }))
}

pub fn entries(
    c: &Campaign,
    snapshots: &BTreeMap<String, Snapshot>,
    now: DateTime<Utc>,
) -> io::Result<Vec<DriftEntry>> {
    let names = c.eligible(AttemptKind::Verify, None)?;
    let expected: Toplevels = c
        .target
        .hosts
        .iter()
        .map(|(name, target)| (name.clone(), target.system_toplevel.clone()))
        .collect::<BTreeMap<_, _>>()
        .into();
    let mut entries = drift::compare(
        &names,
        snapshots,
        Some(&expected),
        Some(drift::default_max_age()),
        now,
    );
    for entry in &mut entries {
        if matches!(entry.status, DriftStatus::InSync | DriftStatus::Activated) {
            if entry.booted.as_ref().is_none_or(|path| path.is_empty())
                || entry.system_profile.is_none()
            {
                entry.status = DriftStatus::Incomplete;
            } else if entry.system_profile != entry.expected {
                entry.status = DriftStatus::Drift;
            }
        }
    }
    Ok(entries)
}

pub fn complete(c: &Campaign, entries: &[DriftEntry]) -> bool {
    let Ok(eligible) = c.eligible(AttemptKind::Verify, None) else {
        return false;
    };
    let names: BTreeSet<_> = entries.iter().map(|entry| entry.host.clone()).collect();
    !entries.is_empty()
        && entries.len() == names.len()
        && names == eligible.into_iter().collect()
        && !c
            .attempts
            .iter()
            .any(|a| a.kind != AttemptKind::Verify && a.state != AttemptState::Settled)
        && entries.iter().all(|entry| {
            matches!(entry.status, DriftStatus::InSync | DriftStatus::Activated)
                && entry.booted.as_ref().is_some_and(|path| !path.is_empty())
                && entry.system_profile == entry.expected
        })
}

fn summary(entries: &[DriftEntry]) -> BTreeMap<&str, usize> {
    let mut out = BTreeMap::new();
    for entry in entries {
        *out.entry(entry.status.as_str()).or_insert(0) += 1;
    }
    out
}

pub fn dual_drift(
    c: &Campaign,
    snapshots: &BTreeMap<String, Snapshot>,
    latest: Option<&Toplevels>,
    latest_source: Option<&SourceIdentity>,
    now: DateTime<Utc>,
) -> io::Result<Value> {
    let campaign = entries(c, snapshots, now)?;
    // A legacy root-revision cache is not a complete nested source identity.
    let latest = latest_source.and(latest);
    let latest_entries = drift::compare(
        &c.eligible(AttemptKind::Verify, None)?,
        snapshots,
        latest,
        Some(drift::default_max_age()),
        now,
    );
    let source = latest_source.map(source_summary).transpose()?;
    Ok(json!({
        "ok":true,"campaign_id":c.campaign_id,"target_digest":c.target_digest,
        "campaign":{"basis":"bound-campaign","source":source_summary(&c.target.source)?,"entries":campaign,"summary":summary(&campaign),"total":campaign.len(),"complete":complete(c,&campaign)},
        "latest_source":{"basis":"latest-evaluated-source","source":source,"expected_source":if latest_source.is_some()&&latest.is_some(){"eval"}else{"none"},"entries":latest_entries,"summary":summary(&latest_entries),"total":latest_entries.len()},
        "exclusions":c.effective_exclusions(),"original_count":c.target.selection.members.len(),"eligible_count":campaign.len(),
    }))
}

/// Only still-staged members need rebooting. Freshly surveyed, already-running
/// members are omitted; mismatched/unknown profiles remain explicitly blocked.
pub fn reboot_candidates(
    c: &Campaign,
    snapshots: &BTreeMap<String, Snapshot>,
    narrow: Option<&BTreeSet<String>>,
    now: DateTime<Utc>,
) -> io::Result<(BTreeSet<String>, BTreeMap<String, String>)> {
    let eligible: BTreeSet<_> = c
        .eligible(AttemptKind::Reboot, narrow)?
        .into_iter()
        .collect();
    let mut names = BTreeSet::new();
    let mut blocked = BTreeMap::new();
    for entry in entries(c, snapshots, now)? {
        if !eligible.contains(&entry.host) {
            continue;
        }
        if entry.system_profile != entry.expected {
            blocked.insert(
                entry.host,
                format!("{}; installed-profile-mismatch-or-unknown", entry.status),
            );
        } else if entry.status == DriftStatus::RebootPending {
            names.insert(entry.host);
        } else if !matches!(entry.status, DriftStatus::InSync | DriftStatus::Activated) {
            blocked.insert(entry.host, entry.status.as_str().into());
        } else if entry.booted.as_ref().is_none_or(|path| path.is_empty()) {
            blocked.insert(entry.host, "boot-state-unknown".into());
        }
    }
    Ok((names, blocked))
}
