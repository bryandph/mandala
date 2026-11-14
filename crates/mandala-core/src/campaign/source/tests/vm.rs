use super::*;
use crate::campaign::{api, execution, views};

pub(super) fn write_vm_evidence(
    run: &deploy::RegisteredDeployRun,
    unit: &crate::vm::OwnerUnit,
    profile: &Path,
) {
    fn remap<T: Clone>(
        values: &BTreeMap<String, T>,
        unit: &crate::vm::OwnerUnit,
    ) -> BTreeMap<String, T> {
        unit.application
            .guests
            .keys()
            .cloned()
            .zip(values.values().cloned())
            .collect()
    }
    let mut journal: crate::vm::owner::Journal =
        serde_json::from_str(include_str!("../../../vm/fixtures/confirmed.json")).unwrap();
    journal.owner = unit.owner.clone();
    journal.transaction = run.run_id.clone();
    journal.unit = Some(unit.clone());
    journal.profile = Some(profile.display().to_string());
    let manifest = journal.manifest.as_mut().unwrap();
    manifest.owner = unit.owner.clone();
    manifest.owner_generation = profile.display().to_string();
    manifest.guests = remap(&manifest.guests, unit);
    let previous = journal.previous.as_mut().unwrap();
    previous.guests = remap(&previous.guests, unit);
    let observed = journal.observed.as_mut().unwrap();
    observed.guests = remap(&observed.guests, unit);
    observed.owner_profile = profile.display().to_string();
    observed.owner_running = profile.display().to_string();
    observed.boot_default = profile.display().to_string();
    journal.guests = unit
        .affected_guests
        .iter()
        .map(|n| (n.clone(), crate::vm::owner::GuestOutcome::Confirmed))
        .collect();
    std::fs::create_dir_all(run.path.join("vm")).unwrap();
    std::fs::write(
        run.path.join("vm").join(format!("{}.json", unit.owner)),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
}

#[test]
fn vm_owner_backend_campaign_preflight_conflicts_and_eval_errors_precede_archive_roots() {
    for (selector, evaluation_error) in [
        ("node-b,!guest-d", false),
        ("node-a,!node-b", false),
        ("node-a", true),
    ] {
        let f = Fixture::new();
        f.fake.borrow_mut().vm = true;
        f.fake.borrow_mut().preflight_failure = evaluation_error;
        let error = Capture::new("fixture:acme", selector, "operator-alice", 2, false)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(if evaluation_error {
                "live preflight evaluation failed"
            } else {
                "conflicts"
            }),
            "{error}"
        );
        let fake = f.fake.borrow();
        assert_eq!(fake.archives, 0);
        assert_eq!(fake.calls.len(), 1);
        assert_eq!(fake.calls[0].0, "nix");
        assert!(!f.root.join("campaign-captures").exists());
        assert!(!f.root.join("campaigns").exists());
    }
}

#[test]
fn vm_owner_backend_campaign_frozen_resolution_rejects_preflight_race_and_cleans_roots() {
    let f = Fixture::new();
    f.fake.borrow_mut().frozen_vm = true;
    let error = Capture::new("fixture:acme", "node-a,!node-b", "operator-alice", 2, false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("conflicts"), "{error}");
    assert!(f.fake.borrow().archives > 0);
    assert!(!f.root.join("campaigns").exists());
    if let Ok(entries) = std::fs::read_dir(f.root.join("campaign-captures")) {
        assert_eq!(entries.count(), 0);
    }
    assert!(
        !f.fake
            .borrow()
            .calls
            .iter()
            .any(|(_, args)| args.first().is_some_and(|a| a == "build"))
    );
}

#[test]
fn vm_owner_backend_campaign_reboot_refuses_unserialized_wrapper_before_survey_or_run() {
    for selector in ["node-b", "node-b,node-c"] {
        let f = Fixture::new();
        f.fake.borrow_mut().vm = true;
        let store = Store::new(f.root.join("campaigns"));
        let _runs = registry::test_hooks::install_runs_base(f.root.join("runs"));
        let capture = Capture::new("fixture:acme", selector, "operator-alice", 2, true).unwrap();
        assert_eq!(
            capture.plan.preview(),
            capture
                .target
                .deploy_preview(&capture.plan.targets, false)
                .unwrap()
        );
        let c = capture.bind(&store).unwrap();
        let id = c.campaign_id.clone();
        let confirmation = c
            .required_confirm(AttemptKind::Deploy, &capture.plan.targets, false)
            .unwrap();
        store
            .transaction(&id, |c| {
                let source = c.target.source.clone();
                let number = c
                    .prepare(Dispatch {
                        kind: AttemptKind::Deploy,
                        generation: 0,
                        source: Some(&source),
                        narrow: None,
                        dry_activate: false,
                        confirm: Some(&confirmation),
                    })?
                    .unwrap();
                c.link_run(number, "20261114T120000_000001-123")?;
                c.settle(
                    number,
                    capture
                        .plan
                        .targets
                        .iter()
                        .map(|name| (name.clone(), HostOutcome::RebootPending))
                        .collect(),
                )
            })
            .unwrap();
        f.fake.borrow_mut().calls.clear();
        let request = execution::RebootRequest {
            generation: 1,
            confirm: None,
            narrow: None,
            serial: "1",
            drain: true,
        };
        let executor = TestReboot {
            store: store.clone(),
            id: id.clone(),
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let refusal = execution::reboot_with_executor(
            &store,
            &id,
            request,
            |_| panic!("no VM reboot run may be allocated"),
            &executor,
        )
        .unwrap();
        assert_eq!(refusal["ok"], false);
        assert_eq!(refusal["complete"], false);
        assert!(refusal["reason"].as_str().unwrap().contains("serialized"));
        assert_eq!(
            refusal["vm_guest_evidence"]["node-a"],
            "guest evidence missing"
        );
        assert!(f.fake.borrow().calls.is_empty());
        assert!(!f.root.join("runs").exists());
        assert_eq!(store.load(&id).unwrap().generation, 1);
        assert_eq!(executor.calls.load(Ordering::Relaxed), 0);
        assert_eq!(refusal["vm_owners"], json!(["node-a"]));
        assert!(refusal["reason"].as_str().unwrap().contains("exclude"));
        let verified = api::verify(&store, &id, false).unwrap();
        assert_eq!(verified["ok"], true);
        assert_eq!(verified["complete"], false);
        assert_eq!(
            store.load(&id).unwrap().outcomes["node-a"].outcome,
            HostOutcome::RebootPending
        );
        if selector.contains("node-c") {
            let ordinary = BTreeSet::from(["node-c".into()]);
            let result = execution::reboot_with_executor(
                &store,
                &id,
                execution::RebootRequest {
                    generation: 2,
                    confirm: Some("node-c"),
                    narrow: Some(&ordinary),
                    serial: "2",
                    drain: false,
                },
                |_| Ok(()),
                &executor,
            )
            .unwrap();
            assert_eq!(result["rc"], 0);
            assert_eq!(result["verified"], false);
            assert_eq!(executor.calls.load(Ordering::Relaxed), 1);
            assert_eq!(
                store.load(&id).unwrap().outcomes["node-a"].outcome,
                HostOutcome::RebootPending
            );
        }
    }
}

#[test]
fn vm_owner_backend_campaign_freezes_owner_and_requested_guests_with_versioned_additive_manifest() {
    let f = Fixture::new();
    f.fake.borrow_mut().vm = true;
    let capture =
        Capture::new("fixture:acme", "node-b,node-c", "operator-alice", 2, false).unwrap();
    assert_eq!(capture.plan.targets, ["node-a", "node-c"]);
    let unit = &capture.target.vm_owner_plan.units["node-a"];
    assert_eq!(unit.requested_guests, ["node-b"]);
    assert_eq!(unit.affected_guests, ["guest-d", "node-b"]);
    assert_eq!(unit.potentially_affected_guests, ["guest-d"]);
    assert_eq!(
        capture.target.selection.members,
        ["guest-d", "node-a", "node-b", "node-c"]
    );
    assert_eq!(capture.target.hosts.len(), 2);
    assert!(capture.target.initial_exclusions.is_empty());
    let store = Store::new(f.root.join("campaigns"));
    let c = capture.bind(&store).unwrap();
    assert_eq!(c.schema_version, 2);
    assert_eq!(store.load(&c.campaign_id).unwrap(), c);
    let required = c
        .required_confirm(AttemptKind::Deploy, &capture.plan.targets, false)
        .unwrap();
    assert!(required.starts_with("node-a,node-c@"));
    let review = api::review(
        &store,
        &c.campaign_id,
        0,
        AttemptKind::Deploy,
        None,
        false,
        None,
    )
    .unwrap();
    assert_eq!(review["required_confirm"], required);
    assert_eq!(review["owner_preview"], capture.plan.preview());
    for guest in ["node-b", "guest-d"] {
        assert_eq!(
            api::narrow(&store, &c.campaign_id, Some(guest))
                .unwrap()
                .unwrap(),
            BTreeSet::from(["node-a".into()])
        );
    }
    let mut corrupt = serde_json::to_value(&c).unwrap();
    corrupt["schema_version"] = json!(1);
    assert!(
        serde_json::from_value::<Campaign>(corrupt)
            .unwrap()
            .validate()
            .is_err()
    );
    let legacy = super::super::super::tests::target();
    let old = Campaign::new("c-20261031T120000_000001-999999".into(), legacy, Utc::now()).unwrap();
    assert_eq!(old.schema_version, 1);
    assert!(
        serde_json::to_value(&old).unwrap()["target"]
            .get("vm_owner_plan")
            .is_none()
    );
}

#[tokio::test]
async fn vm_owner_backend_campaign_partial_build_retries_only_failed_owner_with_frozen_guest_intent()
 {
    let f = Fixture::new();
    f.fake.borrow_mut().vm = true;
    let store = Store::new(f.root.join("campaigns"));
    let _runs = registry::test_hooks::install_runs_base(f.root.join("runs"));
    let capture =
        Capture::new("fixture:acme", "node-b,node-c", "operator-alice", 2, false).unwrap();
    let c = capture.bind(&store).unwrap();
    let id = c.campaign_id.clone();
    let executor = TestExecutor {
        store: store.clone(),
        id: id.clone(),
        state: std::sync::Mutex::new(ExecutionState {
            build_failures: BTreeSet::from(["node-a".into()]),
            ..Default::default()
        }),
    };
    let request = |generation, confirm| execution::DeployRequest {
        generation,
        confirm,
        narrow: None,
        dry_activate: false,
        throttle: 2,
        halt_on_build_failure: false,
        publish: Default::default(),
    };
    let token = c
        .required_confirm(
            AttemptKind::Deploy,
            &["node-a".into(), "node-c".into()],
            false,
        )
        .unwrap();
    assert!(
        execution::deploy_with_executor(
            &store,
            &id,
            request(0, Some("node-a,node-c")),
            |_| Ok(()),
            &executor
        )
        .await
        .is_err()
    );
    assert!(!f.root.join("runs").exists());
    let first = execution::deploy_with_executor(
        &store,
        &id,
        request(0, Some(&token)),
        |_| Ok(()),
        &executor,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(first.outcome.rc, 1);
    let after = store.load(&id).unwrap();
    assert_eq!(after.outcomes["node-a"].outcome, HostOutcome::Failed);
    assert_eq!(after.outcomes["node-c"].outcome, HostOutcome::Confirmed);
    let sibling = after.outcomes["node-c"].clone();
    let generation = after.generation;
    let failed = after.eligible(AttemptKind::Deploy, None).unwrap();
    assert_eq!(failed, ["node-a"]);
    let retry = after
        .required_confirm(AttemptKind::Deploy, &failed, false)
        .unwrap();
    assert!(retry.starts_with("node-a@"));
    assert_ne!(retry, token);
    {
        let mut state = executor.state.lock().unwrap();
        state.build_failures.clear();
        state.trace.clear();
    }
    let done = execution::deploy_with_executor(
        &store,
        &id,
        request(generation, Some(&retry)),
        |_| Ok(()),
        &executor,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(done.outcome.rc, 0);
    let final_c = store.load(&id).unwrap();
    assert_eq!(final_c.outcomes["node-c"], sibling);
    assert_eq!(final_c.outcomes["node-a"].outcome, HostOutcome::Confirmed);
    assert!(
        final_c
            .eligible(AttemptKind::Deploy, None)
            .unwrap()
            .is_empty()
    );
    assert!(views::guest_evidence_matches(&final_c, "node-a"));
    let trace = &executor.state.lock().unwrap().trace;
    assert!(trace.iter().all(|(_, name)| name == "node-a"));
    assert_eq!(
        trace.iter().map(|(op, _)| op.as_str()).collect::<Vec<_>>(),
        ["build", "publish", "copy", "activate"]
    );
    let meta = registry::read_meta(&registry::open_run(&done.run_id).unwrap().info.path);
    assert_eq!(meta["vm_owner_plan"]["requestedGuests"], json!(["node-b"]));
}

#[tokio::test]
async fn vm_owner_backend_campaign_owner_only_confirmation_cannot_settle_missing_guest_evidence() {
    let f = Fixture::new();
    f.fake.borrow_mut().vm = true;
    let store = Store::new(f.root.join("campaigns"));
    let _runs = registry::test_hooks::install_runs_base(f.root.join("runs"));
    let capture = Capture::new("fixture:acme", "node-b", "operator-alice", 2, false).unwrap();
    let c = capture.bind(&store).unwrap();
    let id = c.campaign_id.clone();
    let executor = TestExecutor {
        store: store.clone(),
        id: id.clone(),
        state: std::sync::Mutex::new(ExecutionState {
            omit_vm_evidence: true,
            ..Default::default()
        }),
    };
    let required = c
        .required_confirm(AttemptKind::Deploy, &["node-a".into()], false)
        .unwrap();
    let request = execution::DeployRequest {
        generation: 0,
        confirm: Some(&required),
        narrow: None,
        dry_activate: false,
        throttle: 2,
        halt_on_build_failure: false,
        publish: Default::default(),
    };
    let result = execution::deploy_with_executor(&store, &id, request, |_| Ok(()), &executor)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.outcome.rc, 1);
    let c = store.load(&id).unwrap();
    assert_eq!(c.attempts[0].state, AttemptState::Unknown);
    assert!(c.outcomes.is_empty());
    assert!(c.ensure_idle(c.generation).is_err());
    let view = views::inspect(&store, Some(&id), 0, 100).unwrap();
    assert_eq!(
        view["vm_guest_evidence"]["node-a"],
        "guest evidence missing"
    );
    let system = c.target.hosts["node-a"].system_toplevel.clone();
    let now = Utc::now();
    let snapshots=BTreeMap::from([("node-a".into(),serde_json::from_value(json!({"current":system,"booted":system,"system_profile":system,"captured_at":now.to_rfc3339(),"current_boot":{},"booted_boot":{}})).unwrap())]);
    let entries = views::entries(&c, &snapshots, now).unwrap();
    assert_eq!(entries[0].status, crate::drift::DriftStatus::Incomplete);
    assert!(!views::complete(&c, &entries));
}
