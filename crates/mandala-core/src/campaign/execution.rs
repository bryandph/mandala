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

pub(super) trait Executor: Sync {
    fn build(
        &self,
        run: &deploy::RegisteredDeployRun,
        resolved: &[(String, String)],
    ) -> Result<deploy::BuiltProfiles, deploy::BuildError>;
    fn publish(
        &self,
        run: &deploy::RegisteredDeployRun,
        built: &deploy::BuiltProfiles,
    ) -> io::Result<()>;
    fn fan_out<'a>(
        &'a self,
        run: &'a deploy::RegisteredDeployRun,
        built: &'a deploy::BuiltProfiles,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = io::Result<deploy::DeployOutcome>> + Send + 'a>,
    >;
}
struct NativeExecutor;
impl Executor for NativeExecutor {
    fn build(
        &self,
        run: &deploy::RegisteredDeployRun,
        resolved: &[(String, String)],
    ) -> Result<deploy::BuiltProfiles, deploy::BuildError> {
        deploy::build_bound_profiles(run, resolved)
    }
    fn publish(
        &self,
        run: &deploy::RegisteredDeployRun,
        built: &deploy::BuiltProfiles,
    ) -> io::Result<()> {
        crate::engines::cache_publish::publish(run, built, std::ffi::OsStr::new("nix"))
    }
    fn fan_out<'a>(
        &'a self,
        run: &'a deploy::RegisteredDeployRun,
        built: &'a deploy::BuiltProfiles,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = io::Result<deploy::DeployOutcome>> + Send + 'a>,
    > {
        Box::pin(deploy::deploy_hosts(run, built))
    }
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
    deploy_notified(store, id, request, |_| Ok(())).await
}

pub async fn deploy_notified(
    store: &Store,
    id: &str,
    request: DeployRequest<'_>,
    notify: impl FnOnce(&str) -> io::Result<()>,
) -> io::Result<Option<Deployment>> {
    deploy_with_executor(store, id, request, notify, &NativeExecutor).await
}

pub(super) async fn deploy_with_executor(
    store: &Store,
    id: &str,
    request: DeployRequest<'_>,
    notify: impl FnOnce(&str) -> io::Result<()>,
    executor: &dyn Executor,
) -> io::Result<Option<Deployment>> {
    let current = store.load(id)?;
    current.ensure_idle(request.generation)?;
    if current
        .eligible(AttemptKind::Deploy, request.narrow)?
        .is_empty()
    {
        return Ok(None);
    }
    source::available(&current.target)?;
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
    notify(&run.run_id)?;
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
    let built = match executor.build(&run, &resolved) {
        Ok(built) => built,
        Err(error) => {
            if let Err(event_error) = crate::engines::cache_publish::build_aborted(&run) {
                deploy::record_controller_failure(&run, &event_error.to_string());
            }
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
    // Preserve the native build -> advisory cache publish -> host effects
    // barrier, including dry activation and partial-build recovery.
    if let Err(error) = executor.publish(&run, &built) {
        deploy::record_controller_failure(&run, &error.to_string());
        let evidence = run
            .plan
            .targets
            .iter()
            .map(|name| (name.clone(), HostOutcome::Pending))
            .collect();
        store.transaction(id, |campaign| campaign.settle(number, evidence))?;
        return Err(error);
    }
    let outcome = match executor.fan_out(&run, &built).await {
        Ok(outcome) => outcome,
        Err(error) => {
            deploy::record_controller_failure(&run, &error.to_string());
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

pub struct RebootRequest<'a> {
    pub generation: u64,
    pub confirm: Option<&'a str>,
    pub narrow: Option<&'a BTreeSet<String>>,
    pub serial: &'a str,
    pub drain: bool,
}

pub(super) trait RebootExecutor {
    fn survey(&self, store: &Store, c: &Campaign, names: &[String]) -> io::Result<api::Survey>;
    fn run(
        &self,
        argv: &[String],
        cwd: &Path,
        log: &File,
        on_pid: &mut dyn FnMut(u32) -> io::Result<()>,
    ) -> io::Result<(i32, Option<String>)>;
}
struct NativeReboot;
impl RebootExecutor for NativeReboot {
    fn survey(&self, store: &Store, c: &Campaign, names: &[String]) -> io::Result<api::Survey> {
        api::survey(store, c, names)
    }
    fn run(
        &self,
        argv: &[String],
        cwd: &Path,
        log: &File,
        on_pid: &mut dyn FnMut(u32) -> io::Result<()>,
    ) -> io::Result<(i32, Option<String>)> {
        let mut command = std::process::Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log.try_clone()?);
        match mandala_deploy::process::spawn_std(&mut command) {
            Ok((mut child, mut group)) => {
                on_pid(child.id())?;
                Ok((
                    mandala_deploy::process::wait_std(&mut child, &mut group)?
                        .code()
                        .unwrap_or(1),
                    None,
                ))
            }
            Err(error) => Ok((127, Some(error.to_string()))),
        }
    }
}

pub fn review_reboot(store: &Store, id: &str, request: &RebootRequest<'_>) -> io::Result<Value> {
    review_reboot_with(store, id, request, &NativeReboot)
}

fn review_reboot_with(
    store: &Store,
    id: &str,
    request: &RebootRequest<'_>,
    executor: &dyn RebootExecutor,
) -> io::Result<Value> {
    let c = store.load(id)?;
    c.ensure_idle(request.generation)?;
    let amount = request.serial.trim_end_matches('%').parse::<u32>().ok();
    if !amount.is_some_and(|amount| amount > 0 && (!request.serial.ends_with('%') || amount <= 100))
        || request.serial.contains("%%")
    {
        return Err(invalid(
            "reboot serial must be a positive count or percentage",
        ));
    }
    let names = c.eligible(AttemptKind::Reboot, request.narrow)?;
    if names.is_empty() {
        return Ok(
            serde_json::json!({"ok":true,"no_op":true,"campaign_id":id,"generation":c.generation,"names":[],"required_confirm":""}),
        );
    }
    source::available(&c.target)?;
    let observed = source::observe(&c.target.source.checkout).map_err(|error| {
        invalid(format!(
            "source identity unavailable; create a new campaign: {error}"
        ))
    })?;
    c.ensure_source(Some(&observed))?;
    let survey = executor.survey(store, &c, &names)?;
    if survey.rc != 0 {
        return Ok(
            serde_json::json!({"ok":false,"refused":true,"reason":"fresh reboot survey failed","survey":survey}),
        );
    }
    let snapshots = drift::read_snapshots(&store.state_dir());
    let (names, blocked) = views::reboot_candidates(&c, &snapshots, request.narrow, Utc::now())?;
    let required = names.iter().cloned().collect::<Vec<_>>().join(",");
    let confirmed = names.is_empty() || request.confirm == Some(required.as_str());
    Ok(
        serde_json::json!({"ok":confirmed,"refused":!confirmed,"confirmation_required":!confirmed,"campaign_id":id,"generation":c.generation,"target_digest":c.target_digest,"names":names,"blocked":blocked,"required_confirm":required,"reason":if confirmed{"reviewed"}else{"reboot requires exact eligible confirmation"},"no_op":names.is_empty()}),
    )
}

pub fn reboot(
    store: &Store,
    id: &str,
    request: RebootRequest<'_>,
    notify: impl FnOnce(&str) -> io::Result<()>,
) -> io::Result<Value> {
    reboot_with_executor(store, id, request, notify, &NativeReboot)
}

pub(super) fn reboot_with_executor(
    store: &Store,
    id: &str,
    request: RebootRequest<'_>,
    notify: impl FnOnce(&str) -> io::Result<()>,
    executor: &dyn RebootExecutor,
) -> io::Result<Value> {
    let mut result = review_reboot_with(store, id, &request, executor)?;
    if result["ok"] != true || result["no_op"] == true {
        return Ok(result);
    }
    let names: BTreeSet<String> =
        serde_json::from_value(result["names"].clone()).map_err(invalid_json)?;
    let c = store.load(id)?;
    let observed = source::observe(&c.target.source.checkout)?;
    let number = store
        .transaction(id, |c| {
            c.prepare(Dispatch {
                kind: AttemptKind::Reboot,
                generation: request.generation,
                source: Some(&observed),
                narrow: Some(&names),
                dry_activate: false,
                confirm: request.confirm,
            })
        })?
        .ok_or_else(|| invalid("reboot became empty before dispatch"))?;
    let (run_id, path) = registry::new_run_dir()?;
    let argv = crate::runner::reboot_argv(
        Path::new(&c.target.source.snapshot.store_path),
        &names.iter().cloned().collect::<Vec<_>>().join(","),
        request.serial,
        request.drain,
    )
    .ok_or_else(|| invalid("reboot wrapper unavailable"))?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.join(crate::runner::COMMAND_LOG))?;
    let mut fields = metadata(&store.load(id)?, number)?;
    fields.extend(serde_json::json!({"run_id":run_id,"kind":"reboot","pid":std::process::id(),"argv":argv,"targets":names,"started_at":Utc::now().timestamp(),"supervisor_pid":crate::supervisor::captured_pid()}).as_object().unwrap().clone());
    registry::write_meta(&path, &fields)?;
    store.transaction(id, |c| c.link_run(number, &run_id))?;
    notify(&run_id)?;
    let cwd = crate::runner::ansible_dir(&c.target.source.snapshot.store_path);
    let mut on_pid = |pid| {
        registry::update_meta(
            &path,
            serde_json::json!({"pid":pid,"cwd":cwd})
                .as_object()
                .unwrap()
                .clone(),
        )
    };
    let (rc, error) = match executor.run(&argv, &cwd, &log, &mut on_pid) {
        Ok(result) => result,
        Err(error) => {
            let _ = registry::update_meta(
                &path,
                serde_json::json!({"error":error.to_string()})
                    .as_object()
                    .unwrap()
                    .clone(),
            );
            store.transaction(id, |c| {
                c.attempts[(number - 1) as usize].state = AttemptState::Unknown;
                Ok(())
            })?;
            return Err(error);
        }
    };
    if let Some(error) = error {
        registry::update_meta(
            &path,
            serde_json::json!({"error":error})
                .as_object()
                .unwrap()
                .clone(),
        )?;
    }
    registry::update_meta(
        &path,
        serde_json::json!({"rc":rc,"finished_at":Utc::now().timestamp()})
            .as_object()
            .unwrap()
            .clone(),
    )?;
    store.transaction(id, |c| {
        c.attempts[(number - 1) as usize].state = AttemptState::Settled;
        Ok(())
    })?;
    result["run_id"] = Value::from(run_id);
    result["rc"] = Value::from(rc);
    result["ok"] = Value::Bool(rc == 0);
    result["verified"] = Value::Bool(false);
    Ok(result)
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
    reconcile_checked(store, id, None)
}

pub fn reconcile_at(store: &Store, id: &str, generation: u64) -> io::Result<()> {
    reconcile_checked(store, id, Some(generation))
}

fn reconcile_checked(store: &Store, id: &str, generation: Option<u64>) -> io::Result<()> {
    store.transaction(id, |campaign| {
        if generation.is_some_and(|generation| generation != campaign.generation) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "stale campaign generation",
            ));
        }
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
        if registry::pid_alive(observed.info.pid()) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "linked campaign process is still live",
            ));
        }
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
        if attempt.kind == AttemptKind::Reboot
            && observed
                .info
                .meta
                .get("rc")
                .and_then(Value::as_i64)
                .is_some()
        {
            campaign.attempts[(attempt.number - 1) as usize].state = AttemptState::Settled;
            return Ok(());
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
