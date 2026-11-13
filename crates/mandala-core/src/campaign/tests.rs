use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

const ID: &str = "c-20260102T030405_000001-123";
const RUN: &str = "20260102T030406_000001-123";
const RUN2: &str = "20260102T030407_000001-123";

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "mandala-campaign-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn store(&self) -> Store {
        Store::new(self.0.join("campaigns"))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn now() -> DateTime<Utc> {
    "2026-01-02T03:04:05Z".parse().unwrap()
}
fn node(name: &str) -> SourceNode {
    SourceNode {
        store_path: format!("/nix/store/{name}"),
        content_digest: format!("sha256:{name}"),
        revision: Some("revision-a".into()),
    }
}
pub(super) fn target() -> Target {
    let host = HostTarget {
        profile: "/nix/store/profile-a".into(),
        system_toplevel: "/nix/store/system-a".into(),
        derivation: "/nix/store/profile-a.drv".into(),
        system_derivation: "/nix/store/system-a.drv".into(),
        output: "out".into(),
        settings: serde_json::json!({"hostname":"node-a","activation":"switch"}),
        activation: ActivationMode::Switch,
    };
    Target {
        source: SourceIdentity {
            checkout: "/fixtures/acme".into(),
            revision: Some("revision-a".into()),
            dirty: false,
            root: node("source-a"),
            graph_digest: "sha256:graph-a".into(),
            snapshot: node("capsule-a"),
            lock_digest: "sha256:lock-a".into(),
            inputs: BTreeMap::from([("toolchain/nixpkgs".into(), node("input-a"))]),
            submodules: BTreeMap::new(),
        },
        selection: Selection {
            selector: "@all".into(),
            members: vec!["node-a".into(), "node-b".into(), "node-c".into()],
            deploy_members: vec!["node-a".into(), "node-b".into(), "node-c".into()],
        },
        hosts: BTreeMap::from([
            ("node-a".into(), host.clone()),
            ("node-b".into(), host.clone()),
            ("node-c".into(), host),
        ]),
        initial_exclusions: BTreeMap::new(),
    }
}
fn campaign() -> Campaign {
    Campaign::new(ID.into(), target(), now()).unwrap()
}
fn prepare(c: &mut Campaign, kind: AttemptKind, dry: bool) -> u64 {
    let source = c.target.source.clone();
    let required = c.eligible(kind, None).unwrap().join(",");
    c.prepare(Dispatch {
        kind,
        generation: c.generation,
        source: Some(&source),
        narrow: None,
        dry_activate: dry,
        confirm: Some(&required),
    })
    .unwrap()
    .unwrap()
}
fn settle(c: &mut Campaign, outcomes: [HostOutcome; 3], dry: bool) {
    let number = prepare(c, AttemptKind::Deploy, dry);
    c.link_run(number, RUN).unwrap();
    c.settle(
        number,
        ["node-a", "node-b", "node-c"]
            .into_iter()
            .zip(outcomes)
            .map(|(n, o)| (n.into(), o))
            .collect(),
    )
    .unwrap();
}
fn update(add: &[(&str, &str)], remove: &[&str]) -> ExclusionUpdate {
    ExclusionUpdate {
        generation: 0,
        timestamp: now(),
        actor: "operator-alice".into(),
        reason: "maintenance policy".into(),
        add: add
            .iter()
            .map(|(n, r)| (n.to_string(), r.to_string()))
            .collect(),
        remove: remove.iter().map(|n| n.to_string()).collect(),
    }
}

#[test]
fn generated_id_confinement_rejects_traversal_and_wrong_shapes() {
    assert!(is_valid_id(ID));
    for id in [
        "../campaign",
        "/tmp/campaign",
        "c-20261302T030405_000001-123",
        "c-20260102T030405_000001-0",
        "c-20260102T030405_000001-123/other",
        RUN,
    ] {
        assert!(!is_valid_id(id), "{id}");
    }
}

#[test]
fn canonical_manifest_has_sorted_objects_no_newline_and_golden_bytes() {
    let mut c = campaign();
    c.target.hosts.clear();
    c.target.selection.members.clear();
    c.target.selection.deploy_members.clear();
    c.target_digest = c.target.digest().unwrap();
    let bytes = canonical(&c).unwrap();
    assert_eq!(
        bytes,
        include_bytes!("../../tests/fixtures/campaign-v1.json")
    );
    assert!(!bytes.ends_with(b"\n"));
    assert_eq!(
        digest(b"abc"),
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(serde_json::from_slice::<Campaign>(&bytes).unwrap(), c);
}

#[test]
fn target_validation_refuses_unknown_outputs_and_invalid_members() {
    for field in [
        "profile",
        "system",
        "derivation",
        "output",
        "settings",
        "membership",
    ] {
        let mut t = target();
        let host = t.hosts.get_mut("node-a").unwrap();
        match field {
            "profile" => host.profile.clear(),
            "system" => host.system_toplevel = "/nix/store/../escape".into(),
            "derivation" => host.derivation = "/nix/store/not-a-derivation".into(),
            "output" => host.output = "out^bad".into(),
            "settings" => host.settings = Value::Null,
            _ => t.selection.members.reverse(),
        }
        assert!(t.validate().is_err(), "{field}");
    }
}

#[test]
fn source_identity_uses_dirty_content_and_recursive_inputs_not_revision_labels() {
    let source = target().source;
    let mut changed = source.clone();
    changed.dirty = true;
    changed.revision = None;
    assert!(source.same_content(&changed));
    changed.root.content_digest = "sha256:different-dirty-content".into();
    assert!(!source.same_content(&changed));
    changed = source.clone();
    changed
        .inputs
        .get_mut("toolchain/nixpkgs")
        .unwrap()
        .content_digest = "sha256:nested-moved".into();
    assert!(!source.same_content(&changed));
    changed = source.clone();
    changed.inputs.clear();
    assert!(!source.same_content(&changed));
}

#[test]
fn recursive_submodule_checkout_movement_changes_source_identity() {
    let mut source = target().source;
    source.submodules.insert(
        "vendor/tool".into(),
        SubmoduleIdentity {
            gitlink_revision: "a".into(),
            checkout_revision: "a".into(),
            dirty: false,
            content: node("module-a"),
        },
    );
    let mut changed = source.clone();
    changed
        .submodules
        .get_mut("vendor/tool")
        .unwrap()
        .checkout_revision = "b".into();
    assert!(!source.same_content(&changed));
}

#[test]
fn preview_binding_is_inert_and_creates_no_attempt_or_run_directory() {
    let tmp = Temp::new();
    let c = tmp.store().create(target(), now()).unwrap();
    assert!(c.attempts.is_empty());
    assert!(c.outcomes.is_empty());
    assert!(!tmp.0.join("runs").exists());
    assert_eq!(tmp.store().load(&c.campaign_id).unwrap(), c);
}

#[test]
fn real_dispatch_requires_exact_confirmation_and_rejects_source_movement() {
    let mut c = campaign();
    let source = c.target.source.clone();
    for confirm in [
        None,
        Some("@all"),
        Some("node-a"),
        Some("node-c,node-b,node-a"),
    ] {
        assert_eq!(
            c.prepare(Dispatch {
                kind: AttemptKind::Deploy,
                generation: 0,
                source: Some(&source),
                narrow: None,
                dry_activate: false,
                confirm
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(c.attempts.is_empty());
    }
    let mut moved = source.clone();
    moved
        .inputs
        .get_mut("toolchain/nixpkgs")
        .unwrap()
        .content_digest = "sha256:other".into();
    for observed in [None, Some(&moved)] {
        assert!(
            c.prepare(Dispatch {
                kind: AttemptKind::Deploy,
                generation: 0,
                source: observed,
                narrow: None,
                dry_activate: false,
                confirm: Some("node-a,node-b,node-c")
            })
            .is_err()
        );
        assert!(c.attempts.is_empty());
    }
    prepare(&mut c, AttemptKind::Deploy, false);
    assert_eq!(c.attempts[0].state, AttemptState::Prepared);
    assert_eq!(c.attempts[0].run_id, None);
}

#[test]
fn retry_preserves_confirmed_and_staged_success_with_provenance() {
    let mut c = campaign();
    settle(
        &mut c,
        [
            HostOutcome::Confirmed,
            HostOutcome::Failed,
            HostOutcome::RebootPending,
        ],
        false,
    );
    assert_eq!(c.eligible(AttemptKind::Deploy, None).unwrap(), ["node-b"]);
    assert_eq!(c.eligible(AttemptKind::Reboot, None).unwrap(), ["node-c"]);
    assert_eq!(
        c.eligible(AttemptKind::Verify, None).unwrap(),
        ["node-a", "node-b", "node-c"]
    );
    let n = prepare(&mut c, AttemptKind::Deploy, false);
    c.link_run(n, RUN2).unwrap();
    c.settle(
        n,
        BTreeMap::from([("node-b".into(), HostOutcome::Confirmed)]),
    )
    .unwrap();
    assert!(c.eligible(AttemptKind::Deploy, None).unwrap().is_empty());
    assert_eq!(c.outcomes["node-a"].run_id, RUN);
    assert_eq!(c.outcomes["node-b"].run_id, RUN2);
    assert_eq!(c.outcomes["node-c"].outcome, HostOutcome::RebootPending);
    assert_eq!(c.attempts.len(), 2);
}

#[test]
fn dry_retry_does_not_turn_preview_into_real_success() {
    let mut c = campaign();
    settle(&mut c, [HostOutcome::Confirmed; 3], true);
    assert!(c.outcomes.is_empty());
    assert_eq!(c.eligible(AttemptKind::Deploy, None).unwrap().len(), 3);
    c.validate().unwrap();
}

#[test]
fn selector_can_only_narrow_the_bound_membership() {
    let c = campaign();
    assert_eq!(
        c.eligible(
            AttemptKind::Deploy,
            Some(&BTreeSet::from(["node-b".into()]))
        )
        .unwrap(),
        ["node-b"]
    );
    assert!(
        c.eligible(
            AttemptKind::Deploy,
            Some(&BTreeSet::from(["node-new".into()]))
        )
        .is_err()
    );
}

#[test]
fn prepared_or_running_ownership_blocks_retry_close_and_policy_update() {
    let mut c = campaign();
    prepare(&mut c, AttemptKind::Deploy, false);
    for linked in [false, true] {
        if linked {
            c.link_run(1, RUN).unwrap();
        }
        let source = c.target.source.clone();
        assert_eq!(
            c.prepare(Dispatch {
                kind: AttemptKind::Deploy,
                generation: c.generation,
                source: Some(&source),
                narrow: None,
                dry_activate: true,
                confirm: None
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(
            c.close(
                c.generation,
                Closure {
                    closed_at: now(),
                    actor: "operator-alice".into(),
                    reason: "cancelled".into()
                }
            )
            .is_err()
        );
        assert!(
            c.update_exclusions(c.generation, update(&[("node-b", "maintenance")], &[]))
                .is_err()
        );
    }
    assert!(c.link_run(1, RUN2).is_err());
}

#[test]
fn uncertain_or_incomplete_evidence_never_settles_an_attempt() {
    let mut c = campaign();
    prepare(&mut c, AttemptKind::Deploy, false);
    assert!(c.settle(1, BTreeMap::new()).is_err());
    c.link_run(1, RUN).unwrap();
    assert!(
        c.settle(
            1,
            BTreeMap::from([("node-a".into(), HostOutcome::Confirmed)])
        )
        .is_err()
    );
    assert_eq!(c.attempts[0].state, AttemptState::Running);
    c.attempts[0].state = AttemptState::Unknown;
    assert!(
        c.update_exclusions(c.generation, update(&[], &["node-b"]))
            .is_err()
    );
}

#[test]
fn exclusions_are_durable_generation_checked_and_never_erase_success() {
    let mut c = campaign();
    settle(
        &mut c,
        [
            HostOutcome::Confirmed,
            HostOutcome::Failed,
            HostOutcome::Pending,
        ],
        false,
    );
    let gen0 = c.generation;
    c.update_exclusions(gen0, update(&[("node-b", "maintenance")], &[]))
        .unwrap();
    assert_eq!(c.eligible(AttemptKind::Deploy, None).unwrap(), ["node-c"]);
    assert_eq!(c.effective_exclusions()["node-b"].reason, "maintenance");
    assert!(
        c.update_exclusions(gen0, update(&[("node-c", "other")], &[]))
            .is_err()
    );
    c.update_exclusions(
        c.generation,
        update(&[("node-a", "inspection")], &["node-b"]),
    )
    .unwrap();
    c.update_exclusions(c.generation, update(&[], &["node-a"]))
        .unwrap();
    assert_eq!(
        c.eligible(AttemptKind::Deploy, None).unwrap(),
        ["node-b", "node-c"]
    );
    assert_eq!(c.outcomes["node-a"].outcome, HostOutcome::Confirmed);
    assert_eq!(c.exclusion_updates.len(), 3);
    assert!(
        c.update_exclusions(c.generation, update(&[("new-node", "no")], &[]))
            .is_err()
    );
    assert!(
        c.update_exclusions(c.generation, update(&[("node-b", "")], &[]))
            .is_err()
    );
}

#[test]
fn capability_skips_cannot_be_unskipped_and_all_excluded_is_no_run() {
    let mut t = target();
    t.hosts.remove("node-c");
    t.selection.deploy_members.pop();
    t.initial_exclusions.insert(
        "node-c".into(),
        Exclusion {
            reason: "non-deploy-capable".into(),
            actor: "controller".into(),
            timestamp: now(),
            kind: ExclusionKind::NonDeployCapable,
        },
    );
    let mut c = Campaign::new(ID.into(), t, now()).unwrap();
    assert!(c.update_exclusions(0, update(&[], &["node-c"])).is_err());
    c.update_exclusions(
        0,
        update(&[("node-a", "maintenance"), ("node-b", "maintenance")], &[]),
    )
    .unwrap();
    let source = c.target.source.clone();
    assert_eq!(
        c.prepare(Dispatch {
            kind: AttemptKind::Deploy,
            generation: c.generation,
            source: Some(&source),
            narrow: None,
            dry_activate: false,
            confirm: Some("")
        })
        .unwrap(),
        None
    );
    assert!(c.attempts.is_empty());
}

#[test]
fn close_retains_incomplete_outcomes_and_prevents_future_mutations() {
    let mut c = campaign();
    c.close(
        0,
        Closure {
            closed_at: now(),
            actor: "operator-alice".into(),
            reason: "abandoned".into(),
        },
    )
    .unwrap();
    assert!(c.outcomes.is_empty());
    assert!(c.verification.is_empty());
    assert!(
        c.update_exclusions(c.generation, update(&[], &["node-a"]))
            .is_err()
    );
    assert_eq!(c.eligible(AttemptKind::Verify, None).unwrap().len(), 3);
}

#[test]
fn future_schema_is_identifiable_but_never_mutable() {
    let tmp = Temp::new();
    let store = tmp.store();
    let path = store.path(ID).unwrap();
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join(MANIFEST),
        format!("{{\"campaign_id\":\"{ID}\",\"schema_version\":99}}"),
    )
    .unwrap();
    assert!(matches!(
        store.inspect(ID).unwrap(),
        Inspection::Unsupported {
            schema_version: 99,
            ..
        }
    ));
    assert!(store.transaction(ID, |_| Ok(())).is_err());
}

#[test]
fn corrupt_missing_or_tampered_manifest_never_synthesizes_a_target() {
    let tmp = Temp::new();
    let store = tmp.store();
    assert!(store.load(ID).is_err());
    let path = store.path(ID).unwrap();
    std::fs::create_dir_all(&path).unwrap();
    for bytes in [b"{}".as_slice(), b"not-json", b"{\"schema_version\":1}"] {
        std::fs::write(path.join(MANIFEST), bytes).unwrap();
        assert!(store.load(ID).is_err());
    }
    let mut c = campaign();
    c.target.hosts.get_mut("node-a").unwrap().profile = "/nix/store/retargeted".into();
    std::fs::write(path.join(MANIFEST), canonical(&c).unwrap()).unwrap();
    assert!(store.load(ID).is_err());
}

#[test]
fn unknown_fields_are_ignored_and_failed_transactions_leave_original_bytes() {
    let tmp = Temp::new();
    let store = tmp.store();
    let c = store.create(target(), now()).unwrap();
    let path = store.path(&c.campaign_id).unwrap().join(MANIFEST);
    let mut value = serde_json::to_value(&c).unwrap();
    value["future_note"] = Value::Bool(true);
    std::fs::write(&path, canonical(&value).unwrap()).unwrap();
    assert_eq!(store.load(&c.campaign_id).unwrap(), c);
    let original = std::fs::read(&path).unwrap();
    assert!(
        store
            .transaction(&c.campaign_id, |c| {
                c.target.hosts.clear();
                Ok(())
            })
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(
        store
            .transaction::<()>(&c.campaign_id, |c| {
                c.generation += 1;
                Err(invalid("injected failure"))
            })
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn cross_process_lock_probe() {
    let Some(root) = std::env::var_os("MANDALA_CAMPAIGN_LOCK_TEST_ROOT") else {
        return;
    };
    let store = Store::new(PathBuf::from(root));
    let succeeds = store.lock(ID).is_ok();
    assert_eq!(
        succeeds,
        std::env::var("MANDALA_CAMPAIGN_LOCK_TEST_EXPECT").unwrap() == "available"
    );
}

#[test]
fn os_lock_serializes_independent_processes_not_only_frontend_threads() {
    let tmp = Temp::new();
    let store = tmp.store();
    let guard = store.lock(ID).unwrap();
    let probe = |expect: &str| {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "campaign::tests::cross_process_lock_probe",
                "--nocapture",
            ])
            .env("MANDALA_CAMPAIGN_LOCK_TEST_ROOT", &store.root)
            .env("MANDALA_CAMPAIGN_LOCK_TEST_EXPECT", expect)
            .status()
            .unwrap()
            .success()
    };
    assert!(probe("busy"));
    drop(guard);
    assert!(probe("available"));
}

#[test]
fn shared_lock_excludes_other_frontends_and_releases_on_drop() {
    let tmp = Temp::new();
    let store = tmp.store();
    let guard = store.lock(ID).unwrap();
    assert!(store.lock(ID).is_err());
    drop(guard);
    let _guard = store.lock(ID).unwrap();
}

#[test]
fn stale_generation_refuses_after_another_frontend_commits() {
    let tmp = Temp::new();
    let store = tmp.store();
    let c = store.create(target(), now()).unwrap();
    store
        .transaction(&c.campaign_id, |c| {
            c.update_exclusions(0, update(&[("node-b", "maintenance")], &[]))
        })
        .unwrap();
    assert!(
        store
            .transaction(&c.campaign_id, |c| c
                .update_exclusions(0, update(&[("node-a", "stale")], &[])))
            .is_err()
    );
    assert_eq!(
        store
            .load(&c.campaign_id)
            .unwrap()
            .effective_exclusions()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["node-b"]
    );
}

#[cfg(unix)]
#[test]
fn symlinked_campaign_directories_and_manifests_are_refused() {
    use std::os::unix::fs::symlink;
    let tmp = Temp::new();
    let outside = tmp.0.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let root = tmp.0.join("campaigns");
    symlink(&outside, &root).unwrap();
    assert!(tmp.store().lock(ID).is_err());
    assert!(tmp.store().create(target(), now()).is_err());
    std::fs::remove_file(&root).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    symlink(&outside, root.join(ID)).unwrap();
    assert!(tmp.store().load(ID).is_err());
    std::fs::remove_file(root.join(ID)).unwrap();
    std::fs::create_dir_all(root.join(ID)).unwrap();
    symlink(outside.join("campaign.json"), root.join(ID).join(MANIFEST)).unwrap();
    assert!(tmp.store().load(ID).is_err());
}

fn retained_run(tmp: &Temp, c: &Campaign) -> PathBuf {
    let path = tmp.0.join("runs").join(RUN);
    std::fs::create_dir_all(&path).unwrap();
    registry::write_meta(
        &path,
        &serde_json::json!({"campaign_id":c.campaign_id,"pid":null,"rc":0})
            .as_object()
            .unwrap()
            .clone(),
    )
    .unwrap();
    path
}
fn closed_campaign(store: &Store) -> Campaign {
    let mut c = store.create(target(), now()).unwrap();
    store
        .transaction(&c.campaign_id, |c| {
            settle(c, [HostOutcome::Confirmed; 3], false);
            c.close(
                c.generation,
                Closure {
                    closed_at: now(),
                    actor: "operator-alice".into(),
                    reason: "complete".into(),
                },
            )
        })
        .unwrap();
    c = store.load(&c.campaign_id).unwrap();
    c
}

#[test]
fn open_campaign_retains_manifest_roots_and_linked_runs_past_age_and_count() {
    let tmp = Temp::new();
    let store = tmp.store();
    let c = store.create(target(), now()).unwrap();
    store
        .transaction(&c.campaign_id, |c| {
            settle(c, [HostOutcome::Confirmed; 3], false);
            Ok(())
        })
        .unwrap();
    let path = retained_run(&tmp, &c);
    let campaign_path = store.path(&c.campaign_id).unwrap();
    std::fs::create_dir(campaign_path.join("roots")).unwrap();
    assert!(
        store
            .retain(
                &tmp.0.join("runs"),
                0,
                1,
                now() + chrono::TimeDelta::days(90)
            )
            .contains(RUN)
    );
    assert!(path.exists());
    assert!(campaign_path.join("roots").exists());
}

#[test]
fn closed_campaign_prunes_as_unit_only_after_close_age() {
    let tmp = Temp::new();
    let store = tmp.store();
    let c = closed_campaign(&store);
    let path = retained_run(&tmp, &c);
    let campaign_path = store.path(&c.campaign_id).unwrap();
    store.retain(
        &tmp.0.join("runs"),
        20,
        30,
        now() + chrono::TimeDelta::days(29),
    );
    assert!(path.exists());
    assert!(campaign_path.exists());
    store.retain(
        &tmp.0.join("runs"),
        20,
        30,
        now() + chrono::TimeDelta::days(30),
    );
    assert!(!path.exists());
    assert!(!campaign_path.exists());
}

#[test]
fn live_linked_supervisor_prevents_closed_unit_pruning() {
    let tmp = Temp::new();
    let store = tmp.store();
    let c = closed_campaign(&store);
    let path = retained_run(&tmp, &c);
    registry::update_meta(
        &path,
        serde_json::json!({"supervisor_pid": std::process::id()})
            .as_object()
            .unwrap()
            .clone(),
    )
    .unwrap();
    store.retain(
        &tmp.0.join("runs"),
        0,
        1,
        now() + chrono::TimeDelta::days(90),
    );
    assert!(path.exists());
    assert!(store.path(&c.campaign_id).unwrap().exists());
}

#[test]
fn interrupted_unlinked_allocations_share_closed_retention_and_live_protection() {
    let tmp = Temp::new();
    let store = tmp.store();
    let c = closed_campaign(&store);
    let linked = retained_run(&tmp, &c);
    let orphan = tmp.0.join("runs").join(RUN2);
    std::fs::create_dir_all(&orphan).unwrap();
    registry::write_meta(
        &orphan,
        serde_json::json!({"campaign_id":c.campaign_id,"pid":std::process::id(),"rc":127})
            .as_object()
            .unwrap(),
    )
    .unwrap();
    store.retain(
        &tmp.0.join("runs"),
        0,
        1,
        now() + chrono::TimeDelta::days(90),
    );
    assert!(linked.exists());
    assert!(orphan.exists());
    registry::update_meta(
        &orphan,
        serde_json::json!({"pid":null}).as_object().unwrap().clone(),
    )
    .unwrap();
    store.retain(
        &tmp.0.join("runs"),
        0,
        1,
        now() + chrono::TimeDelta::days(90),
    );
    assert!(!linked.exists());
    assert!(!orphan.exists());
    assert!(!store.path(&c.campaign_id).unwrap().exists());
}

#[test]
fn missing_run_metadata_cannot_prove_a_closed_unit_is_inactive() {
    let tmp = Temp::new();
    let store = tmp.store();
    let c = closed_campaign(&store);
    let path = retained_run(&tmp, &c);
    std::fs::remove_file(path.join("meta.json")).unwrap();
    store.retain(
        &tmp.0.join("runs"),
        0,
        1,
        now() + chrono::TimeDelta::days(90),
    );
    assert!(path.exists());
    assert!(store.path(&c.campaign_id).unwrap().exists());
}
