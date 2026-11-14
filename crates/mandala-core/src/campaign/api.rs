//! Shared lifecycle facade for CLI, TUI and MCP. Frontends may launch a
//! supervised continuation after review; the continuation repeats all checks.

use super::*;
use serde_json::json;

pub fn narrow(
    store: &Store,
    id: &str,
    selector: Option<&str>,
) -> io::Result<Option<BTreeSet<String>>> {
    let Some(selector) = selector else {
        return Ok(None);
    };
    let c = store.load(id)?;
    let inv = if selector.contains('@') {
        source::bound_inventory(&c.target.source)?
    } else {
        let members: BTreeMap<String, Value> = c
            .target
            .selection
            .members
            .iter()
            .map(|name| (name.clone(), json!({"name":name})))
            .collect();
        crate::inventory::Inventory::from_value(
            json!({"schemaVersion":1,"members":members,"groups":{}}),
        )
        .map_err(|error| invalid(error.to_string()))?
    };
    let names = match inv.resolve(selector) {
        Ok(names) => names,
        Err(crate::inventory::InventoryError::ResolvesToNothing(_)) => Vec::new(),
        Err(error) => return Err(invalid(error.to_string())),
    };
    if names
        .iter()
        .any(|name| !c.target.selection.members.contains(name))
    {
        return Err(invalid("campaign selector cannot widen bound membership"));
    }
    Ok(Some(
        names
            .into_iter()
            .filter_map(|name| {
                if c.target.hosts.contains_key(&name) {
                    Some(name)
                } else {
                    c.target
                        .vm_owner_plan
                        .units
                        .iter()
                        .find(|(_, unit)| unit.affected_guests.contains(&name))
                        .map(|(owner, _)| owner.clone())
                }
            })
            .collect(),
    ))
}
pub struct CreateRequest<'a> {
    pub selector: &'a str,
    pub actor: &'a str,
    pub boot: bool,
    pub preview: bool,
    pub confirm: Option<&'a str>,
    pub throttle: i64,
}

pub fn create(store: &Store, flake: &str, request: CreateRequest<'_>) -> io::Result<Value> {
    let capture = source::Capture::new(
        flake,
        request.selector,
        request.actor,
        request.throttle,
        request.boot,
    )?;
    let names = capture.plan.targets.clone();
    let required = capture
        .target
        .required_confirm(AttemptKind::Deploy, &names, false)?;
    if !request.preview && request.confirm != Some(required.as_str()) {
        return owner_preview(
            json!({"ok":false,"refused":true,"confirmation_required":true,"reason":"real creation requires exact eligible confirmation","required_confirm":required,"names":names,"generation":0,"target_digest":capture.target.digest()?,"source":views::source_summary(&capture.target.source)?}),
            &capture.target,
            &names,
            false,
        );
    }
    let c = capture.bind(store)?;
    let mut result = views::inspect(store, Some(&c.campaign_id), 0, 100)?;
    result["preview"] = Value::Bool(request.preview);
    result["ready_to_deploy"] = Value::Bool(!request.preview && !names.is_empty());
    result["required_confirm"] = Value::from(required);
    result["names"] = Value::from(names.clone());
    owner_preview(result, &capture.target, &names, false)
}

fn owner_preview(
    mut result: Value,
    target: &Target,
    names: &[String],
    dry: bool,
) -> io::Result<Value> {
    let plan = execution::target_plan(target, names, dry, 1)?;
    if !plan.vm.is_empty() {
        result["owner_preview"] = plan.preview();
    }
    Ok(result)
}

pub fn inspect(
    store: &Store,
    id: Option<&str>,
    offset: usize,
    limit: usize,
    check_source: bool,
) -> io::Result<Value> {
    let mut result = views::inspect(store, id, offset, limit)?;
    if check_source
        && result["ok"] == true
        && let Some(id) = id
    {
        let c = store.load(id)?;
        match source::observe(&c.target.source.checkout) {
            Ok(observed) => {
                result["source_state"] = Value::from(if c.target.source.same_content(&observed) {
                    "matched"
                } else {
                    "moved"
                });
                result["observed_source"] = views::source_summary(&observed)?;
            }
            Err(error) => {
                result["source_state"] = Value::from("unavailable");
                result["source_error"] = Value::from(error.to_string());
            }
        }
    }
    Ok(result)
}

pub fn review(
    store: &Store,
    id: &str,
    generation: u64,
    kind: AttemptKind,
    narrow: Option<&BTreeSet<String>>,
    dry: bool,
    confirm: Option<&str>,
) -> io::Result<Value> {
    let c = store.load(id)?;
    c.ensure_idle(generation)?;
    let names = c.eligible(kind, narrow)?;
    let required = c.required_confirm(kind, &names, dry)?;
    let mut result = owner_preview(
        json!({"ok":true,"campaign_id":id,"target_digest":c.target_digest,"generation":c.generation,"names":names,"required_confirm":required,"dry_activate":dry}),
        &c.target,
        &names,
        dry,
    )?;
    if names.is_empty() {
        result["no_op"] = Value::Bool(true);
        return Ok(result);
    }
    source::available(&c.target)?;
    match source::observe(&c.target.source.checkout) {
        Ok(observed) => {
            if !c.target.source.same_content(&observed) {
                result["ok"] = Value::Bool(false);
                result["refused"] = Value::Bool(true);
                result["reason"] = Value::from("source changed; create a new campaign");
            }
            result["recorded_source"] = views::source_summary(&c.target.source)?;
            result["observed_source"] = views::source_summary(&observed)?;
        }
        Err(error) => {
            result["ok"] = Value::Bool(false);
            result["refused"] = Value::Bool(true);
            result["reason"] = Value::from("source identity unavailable; create a new campaign");
            result["source_error"] = Value::from(error.to_string());
        }
    }
    if result["ok"] == true
        && (kind == AttemptKind::Reboot
            || !dry
            || !c.target.vm_owner_plan.for_targets(&names).is_empty())
        && confirm != Some(required.as_str())
        && !names.is_empty()
    {
        result["ok"] = Value::Bool(false);
        result["refused"] = Value::Bool(true);
        result["reason"] = Value::from("real mutation requires exact eligible confirmation");
        result["confirmation_required"] = Value::Bool(true);
    }
    Ok(result)
}

pub struct PolicyRequest<'a> {
    pub generation: u64,
    pub actor: &'a str,
    pub reason: &'a str,
    pub confirm: Option<&'a str>,
}

pub fn exclude(
    store: &Store,
    id: &str,
    request: PolicyRequest<'_>,
    add: BTreeMap<String, String>,
    remove: Vec<String>,
) -> io::Result<Value> {
    let PolicyRequest {
        generation,
        actor,
        reason,
        confirm,
    } = request;
    if confirm != Some(id) {
        return Ok(
            json!({"ok":false,"refused":true,"required_confirm":id,"generation":generation}),
        );
    }
    store.transaction(id, |c| {
        c.update_exclusions(
            generation,
            ExclusionUpdate {
                generation,
                timestamp: Utc::now(),
                actor: actor.into(),
                reason: reason.into(),
                add,
                remove,
            },
        )
    })?;
    views::inspect(store, Some(id), 0, 100)
}

pub fn close(
    store: &Store,
    id: &str,
    generation: u64,
    actor: &str,
    reason: &str,
    confirm: Option<&str>,
) -> io::Result<Value> {
    if confirm != Some(id) {
        return Ok(
            json!({"ok":false,"refused":true,"required_confirm":id,"generation":generation}),
        );
    }
    store.transaction(id, |c| {
        c.close(
            generation,
            Closure {
                closed_at: Utc::now(),
                actor: actor.into(),
                reason: reason.into(),
            },
        )
    })?;
    views::inspect(store, Some(id), 0, 100)
}

pub fn drift(store: &Store, id: &str, flake: &str, do_eval: bool) -> io::Result<Value> {
    let c = store.load(id)?;
    let snapshots = drift::read_snapshots(&store.state_dir());
    let mut latest = None;
    let mut source = None;
    let mut error = None;
    let mut moved = None;
    if do_eval {
        match source::ReadSource::capture(flake) {
            Ok(read) => {
                latest = Some(read.expected(&c.target.hosts.keys().cloned().collect::<Vec<_>>()));
                source = Some(read.identity().clone());
                moved = source::observe(flake)
                    .ok()
                    .map(|observed| !read.identity().same_content(&observed));
            }
            Err(e) => error = Some(e.to_string()),
        }
    }
    let mut result =
        views::dual_drift(&c, &snapshots, latest.as_ref(), source.as_ref(), Utc::now())?;
    result["latest_source"]["source_moved_after_evaluation"] =
        serde_json::to_value(moved).map_err(invalid_json)?;
    if let Some(error) = error {
        result["latest_source"]["eval_error"] = Value::from(error);
    }
    Ok(result)
}

#[derive(Debug, Serialize)]
pub struct Survey {
    pub rc: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn survey(store: &Store, c: &Campaign, names: &[String]) -> io::Result<Survey> {
    survey_for_run(store, c, names, None)
}

fn survey_for_run(
    store: &Store,
    c: &Campaign,
    names: &[String],
    run_id: Option<&str>,
) -> io::Result<Survey> {
    let mut command = std::process::Command::new("ansible-playbook");
    command
        .arg("mandala.fleet.state")
        .arg("-l")
        .arg(names.join(","))
        .current_dir(crate::runner::ansible_dir(
            &c.target.source.snapshot.store_path,
        ))
        .env("MANDALA_FLEET_STATE", store.state_dir());
    survey_command(store, run_id, &mut command)
}

fn survey_command(
    store: &Store,
    run_id: Option<&str>,
    command: &mut std::process::Command,
) -> io::Result<Survey> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let (mut child, group) = mandala_deploy::process::spawn_std(command)?;
    if let Some(run) = run_id
        && let Err(error) = registry::update_meta(
            &store.state_dir().join("runs").join(run),
            json!({"pid":child.id()}).as_object().unwrap().clone(),
        )
    {
        drop(group);
        let _ = child.wait();
        return Err(error);
    }
    let output = mandala_deploy::process::wait_std_with_output(child, group)?;
    let bounded = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .rev()
            .take(80)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(Survey {
        rc: output.status.code().unwrap_or(-1),
        stdout: bounded(&output.stdout),
        stderr: bounded(&output.stderr),
    })
}

pub fn verify(store: &Store, id: &str, refresh: bool) -> io::Result<Value> {
    verify_registered(store, id, refresh, |c, names, run| {
        survey_for_run(store, c, names, Some(run))
    })
}

#[cfg(test)]
fn verify_with_survey(
    store: &Store,
    id: &str,
    refresh: bool,
    survey: impl FnOnce(&Campaign, &[String]) -> io::Result<Survey>,
) -> io::Result<Value> {
    verify_registered(store, id, refresh, |c, names, _| survey(c, names))
}

fn verify_registered(
    store: &Store,
    id: &str,
    refresh: bool,
    survey: impl FnOnce(&Campaign, &[String], &str) -> io::Result<Survey>,
) -> io::Result<Value> {
    let initial = store.load(id)?;
    let names = initial.eligible(AttemptKind::Verify, None)?;
    if initial.closure.is_some() || names.is_empty() {
        let entries = views::entries(
            &initial,
            &drift::read_snapshots(&store.state_dir()),
            Utc::now(),
        )?;
        return views::with_vm_evidence(
            json!({
                "ok":true,"campaign_id":id,"closed_read_only":initial.closure.is_some(),
                "no_op":names.is_empty(),"recorded":false,"complete":false,
                "comparison_complete":views::complete(&initial,&entries),
                "entries":entries,"exclusions":initial.effective_exclusions(),
                "original_count":initial.target.selection.members.len(),"eligible_count":names.len(),
                "survey_performed":false,
            }),
            &initial,
        );
    }
    let number = store
        .transaction(id, |c| {
            c.prepare(Dispatch {
                kind: AttemptKind::Verify,
                generation: initial.generation,
                source: None,
                narrow: None,
                dry_activate: false,
                confirm: None,
            })
        })?
        .ok_or_else(|| invalid("verification became empty before dispatch"))?;
    // Registration and linking are serialized with close. No host survey is
    // launched until both sides of the attempt reference are durable.
    let (run_id, path, dispatch) = store.transaction(id, |c| {
        if c.closure.is_some() {
            return Err(invalid("campaign closed before verification registration"));
        }
        let (run_id, path) = registry::new_run_dir_in(&store.state_dir().join("runs"))?;
        let mut fields = execution::metadata(c, number)?;
        fields.extend(
            json!({
                "run_id":run_id,"kind":"verify","pid":std::process::id(),
                "controller_pid":std::process::id(),"targets":names,
                "started_at":Utc::now().timestamp_millis() as f64/1000.0,
                "supervisor_pid":crate::supervisor::captured_pid(),
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        registry::write_meta(&path, &fields)?;
        c.link_run(number, &run_id)?;
        Ok((run_id, path, c.clone()))
    })?;
    let mut survey_result = None;
    let mut survey_error = None;
    if refresh {
        match survey(&dispatch, &names, &run_id) {
            Ok(result) => survey_result = Some(result),
            Err(error) => survey_error = Some(error.to_string()),
        }
    }
    let entries = views::entries(
        &dispatch,
        &drift::read_snapshots(&store.state_dir()),
        Utc::now(),
    )?;
    let survey_ok = !refresh || survey_result.as_ref().is_some_and(|survey| survey.rc == 0);
    let captured_complete = survey_ok && views::complete(&dispatch, &entries);
    let results = entries
        .iter()
        .map(|entry| {
            Ok((
                entry.host.clone(),
                serde_json::to_value(entry).map_err(invalid_json)?,
            ))
        })
        .collect::<io::Result<BTreeMap<_, _>>>()?;
    let generation = dispatch.attempts[(number - 1) as usize].generation;
    let guard = store.lock(id)?;
    let mut latest = store.load(id)?;
    let recorded = latest.closure.is_none();
    let stale = latest.generation != generation;
    let complete = captured_complete && recorded && !stale;
    let report = views::with_vm_evidence(
        json!({
            "ok":survey_ok,"campaign_id":id,"run_id":run_id,"attempt":number,
            "generation":generation,"current_generation":latest.generation,
            "generation_stale":stale,"target_digest":dispatch.target_digest,
            "recorded":recorded,"complete":complete,"captured_complete":captured_complete,
            "entries":entries,"exclusions":dispatch.effective_exclusions(),
            "original_count":dispatch.target.selection.members.len(),"eligible_count":names.len(),
            "survey_performed":refresh,"survey":survey_result,"survey_error":survey_error,
        }),
        &dispatch,
    )?;
    let temporary = path.join("verification.json.tmp");
    let mut file = File::create(&temporary)?;
    file.write_all(&canonical(&report)?)?;
    file.sync_all()?;
    std::fs::rename(temporary, path.join("verification.json"))?;
    File::open(&path)?.sync_all()?;
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.join(crate::runner::COMMAND_LOG))?;
    if let Some(survey) = &survey_result {
        writeln!(log, "{}\n{}", survey.stdout, survey.stderr)?;
    }
    writeln!(
        log,
        "bound verification complete={complete}; generation={generation}; recorded={recorded}"
    )?;
    log.sync_all()?;
    registry::update_meta(
        &path,
        json!({
            "pid":null,"rc":if complete{0}else{1},
            "finished_at":Utc::now().timestamp_millis() as f64/1000.0,
            "verification_file":"verification.json","complete":complete,
        })
        .as_object()
        .unwrap()
        .clone(),
    )?;
    if recorded {
        latest.attempts[(number - 1) as usize].state = AttemptState::Settled;
        latest.verification.push(Verification {
            run_id: run_id.clone(),
            attempt: number,
            generation,
            observed_at: Utc::now(),
            results,
            complete,
        });
        store.write(&guard, &latest)?;
    }
    // If close won the race, the ordinary run retains the complete report.
    // Do not rewrite even identical closed-manifest bytes.
    Ok(report)
}

#[cfg(test)]
mod tests;
