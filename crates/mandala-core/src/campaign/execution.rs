//! Campaign execution adapter onto ordinary registry/deploy runs. Dispatch
//! persists ownership before run allocation and links both sides before effects.
//! Neither restart nor a lost response is permission to launch another attempt.

use super::*;
use crate::engines::deploy;
use crate::runner::HostState;

pub struct DeployRequest<'a> {
    pub generation: u64,
    pub confirm: Option<&'a str>,
    pub narrow: Option<&'a BTreeSet<String>>,
    pub dry_activate: bool,
    pub throttle: i64,
    pub halt_on_build_failure: bool,
    pub publish: crate::engines::cache_publish::Config,
}

pub struct Deployment {
    pub campaign_id: String,
    pub run_id: String,
    pub outcome: deploy::DeployOutcome,
}

fn plan(
    campaign: &Campaign,
    targets: &[String],
    request: &DeployRequest<'_>,
) -> io::Result<deploy::DeployPlan> {
    if request.throttle <= 0 {
        return Err(invalid("campaign throttle must be positive"));
    }
    let mut settings = BTreeMap::new();
    for name in targets {
        let target = &campaign.target.hosts[name];
        let mut value = target.settings.clone();
        value["activation"] = Value::from(match target.activation {
            ActivationMode::Boot => "boot",
            ActivationMode::Switch => "switch",
        });
        settings.insert(name.clone(), value);
    }
    Ok(deploy::DeployPlan {
        limit: targets.join(","),
        targets: targets.to_vec(),
        skipped: Vec::new(),
        settings,
        dry_activate: request.dry_activate,
        boot: false,
        throttle: request.throttle,
        halt_on_build_failure: request.halt_on_build_failure,
        publish: request.publish.clone(),
    })
}

/// A real creation calls this immediately after binding; preview creation does
/// not. Retries use the identical failed/pending-member policy. Empty work is a
/// no-op and never allocates a run. All build inputs come from the manifest.
pub async fn deploy(
    store: &Store,
    id: &str,
    request: DeployRequest<'_>,
) -> io::Result<Option<Deployment>> {
    let current = store.load(id)?;
    if !Path::new(&current.target.source.snapshot.store_path).exists()
        || current
            .target
            .hosts
            .values()
            .any(|host| !Path::new(&host.derivation).is_file())
    {
        return Err(invalid(
            "campaign target-unavailable: recorded source or derivation is missing; no replacement target evaluated",
        ));
    }
    let observed = source::observe(&current.target.source.checkout).map_err(|error| {
        invalid(format!(
            "campaign source identity unavailable; create a new campaign: {error}"
        ))
    })?;
    let prepared = store.transaction(id, |campaign| {
        // Validate options before publishing prepared ownership.
        let eligible = campaign.eligible(AttemptKind::Deploy, request.narrow)?;
        let plan = plan(campaign, &eligible, &request)?;
        let number = campaign.prepare(Dispatch {
            kind: AttemptKind::Deploy,
            generation: request.generation,
            source: Some(&observed),
            narrow: request.narrow,
            dry_activate: request.dry_activate,
            confirm: request.confirm,
        })?;
        Ok(number.map(|number| (number, plan)))
    })?;
    let Some((number, plan)) = prepared else {
        return Ok(None);
    };
    let run = deploy::register_run(plan).map_err(|error| io::Error::other(error.to_string()))?;
    // A crash before linking leaves prepared ownership. Reconciliation may
    // release that ownership only after its recorded owner/supervisor is dead.
    store.transaction(id, |campaign| {
        campaign.link_run(number, &run.run_id)?;
        registry::update_meta(&run.path, metadata(campaign, number)?)
    })?;
    let campaign = store.load(id)?;
    let resolved: Vec<_> = run
        .plan
        .targets
        .iter()
        .map(|name| {
            let host = &campaign.target.hosts[name];
            (
                format!("{}^{}", host.derivation, host.output),
                host.profile.clone(),
            )
        })
        .collect();
    let built = match deploy::build_bound_profiles(&run, &resolved) {
        Ok(built) => built,
        Err(error) => {
            // The build adapter returns before fan-out on a fatal build or
            // cancellation. No activation was scheduled; work stays pending.
            let evidence = run
                .plan
                .targets
                .iter()
                .map(|name| (name.clone(), HostOutcome::Pending))
                .collect();
            store.transaction(id, |campaign| campaign.settle(number, evidence))?;
            return Err(io::Error::other(error.to_string()));
        }
    };
    let outcome = match deploy::deploy_hosts(&run, &built).await {
        Ok(outcome) => outcome,
        Err(error) => {
            store.transaction(id, |campaign| {
                campaign.attempts[(number - 1) as usize].state = AttemptState::Unknown;
                Ok(())
            })?;
            return Err(error);
        }
    };
    let evidence = outcome
        .results
        .iter()
        .map(|(name, result)| {
            outcome_from_state(result.state).map(|outcome| (name.clone(), outcome))
        })
        .collect::<io::Result<BTreeMap<_, _>>>()?;
    store.transaction(id, |campaign| campaign.settle(number, evidence))?;
    Ok(Some(Deployment {
        campaign_id: id.into(),
        run_id: run.run_id,
        outcome,
    }))
}

fn outcome_from_state(state: HostState) -> io::Result<HostOutcome> {
    match state {
        HostState::Confirmed => Ok(HostOutcome::Confirmed),
        HostState::RebootPending => Ok(HostOutcome::RebootPending),
        HostState::Failed => Ok(HostOutcome::Failed),
        HostState::RolledBack => Ok(HostOutcome::RolledBack),
        _ => Err(invalid("campaign run has incomplete host evidence")),
    }
}

pub fn metadata(campaign: &Campaign, number: u64) -> io::Result<registry::Meta> {
    let attempt = campaign
        .attempts
        .iter()
        .find(|a| a.number == number)
        .ok_or_else(|| invalid("campaign attempt not found"))?;
    Ok(serde_json::json!({
        "campaign_id": campaign.campaign_id,
        "campaign_target_digest": campaign.target_digest,
        "campaign_generation": attempt.generation,
        "campaign_attempt": number,
    })
    .as_object()
    .expect("object")
    .clone())
}

/// Reconcile once, read-only with respect to host systems. A live owner or
/// incomplete/uncertain host evidence keeps scheduling blocked. Never relaunch.
pub fn reconcile(store: &Store, id: &str) -> io::Result<()> {
    store.transaction(id, |campaign| {
        let Some(attempt) = campaign
            .attempts
            .iter()
            .find(|attempt| {
                attempt.kind != AttemptKind::Verify && attempt.state != AttemptState::Settled
            })
            .cloned()
        else {
            return Ok(());
        };
        if registry::pid_alive(Some(i64::from(attempt.owner_pid)))
            || registry::pid_alive(attempt.owner_supervisor_pid)
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "campaign owner or supervisor is still live",
            ));
        }
        let Some(run_id) = &attempt.run_id else {
            // Prepared but never linked means the effects barrier was never
            // crossed. Retain the allocation history and pending outcomes.
            campaign.attempts[(attempt.number - 1) as usize].state = AttemptState::Settled;
            return Ok(());
        };
        let mut observed = registry::open_run(run_id).ok_or_else(|| {
            invalid("linked campaign run unavailable; ownership remains uncertain")
        })?;
        let liveness = observed.liveness();
        if matches!(
            liveness,
            registry::RunLiveness::Running | registry::RunLiveness::Unknown
        ) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "linked campaign run is live or uncertain",
            ));
        }
        let evidence = attempt
            .targets
            .iter()
            .map(|name| {
                let host = observed
                    .tailer
                    .hosts
                    .get(name)
                    .ok_or_else(|| invalid("linked run lacks terminal host evidence"))?;
                outcome_from_state(host.state).map(|outcome| (name.clone(), outcome))
            })
            .collect::<io::Result<BTreeMap<_, _>>>()?;
        campaign.settle(attempt.number, evidence)
    })
}

#[cfg(test)]
mod tests;
