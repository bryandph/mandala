use super::*;
use crate::registry::{Meta, ObservedRun, RunInfo, RunLiveness};
use crate::runner::{EventTailer, HostState};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const GOLDEN: &str = include_str!("fixtures/confirmed.json");
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mandala-vm-observations-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join("vm")).unwrap();
        Self(path)
    }
    fn write(&self, journal: &Journal, dry: bool) {
        let unit = journal.unit.clone().unwrap();
        let plan = OwnerPlan {
            selected_members: vec!["guest-a".into()],
            requested_guests: vec!["guest-a".into()],
            units: BTreeMap::from([("host-a".into(), unit)]),
        };
        let mut meta = Meta::new();
        meta.insert("kind".into(), "deploy".into());
        meta.insert("rc".into(), 0.into());
        meta.insert("dry_activate".into(), dry.into());
        let context =
            serde_json::json!({"settings":{},"boot":false,"dryActivate":dry,"targets":["host-a"]});
        meta.insert("targets".into(), context["targets"].clone());
        meta.insert("vm_owner_context".into(), context.clone());
        meta.insert("vm_inventory_identity".into(), "fixture-revision".into());
        meta.insert(
            "vm_owner_approval".into(),
            plan.fingerprint("fixture-revision", &context)
                .unwrap()
                .into(),
        );
        meta.insert("vm_owner_plan".into(), serde_json::to_value(plan).unwrap());
        crate::registry::write_meta(&self.0, &meta).unwrap();
        std::fs::write(
            self.0.join("vm/host-a.json"),
            serde_json::to_vec(journal).unwrap(),
        )
        .unwrap();
        std::fs::write(self.0.join("host-a.jsonl"),"{\"v\":2,\"host\":\"host-a\",\"plugin\":\"deploy\",\"event\":\"milestone\",\"milestone\":\"confirm\"}\n").unwrap();
    }
    fn observe(&self) -> ObservedRun {
        ObservedRun {
            info: RunInfo {
                run_id: "20261114T120000_000001-123".into(),
                path: self.0.clone(),
                meta: crate::registry::read_meta(&self.0),
            },
            tailer: EventTailer::new(&self.0),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn journal() -> Journal {
    serde_json::from_str(GOLDEN).unwrap()
}

#[test]
fn vm_owner_backend_missing_or_changed_approval_context_is_unknown_not_confirmed() {
    let fixture = Fixture::new();
    fixture.write(&journal(), false);
    let original = crate::registry::read_meta(&fixture.0);
    assert_eq!(
        VmRun::read(&fixture.0).owners["host-a"].verdict,
        Verdict::Verified
    );
    for field in [
        "vm_owner_approval",
        "vm_inventory_identity",
        "vm_owner_context",
    ] {
        let mut meta = original.clone();
        meta.remove(field);
        crate::registry::write_meta(&fixture.0, &meta).unwrap();
        let vm = VmRun::read(&fixture.0);
        assert!(vm.unsuccessful());
        assert_eq!(vm.summary.unknown, 2);
        assert_eq!(vm.summary.verified, 0);
        assert!(vm.lines()[0].contains("unavailable"));
    }
    let mut meta = original;
    meta.get_mut("vm_owner_context").unwrap()["settings"] =
        serde_json::json!({"sshUser":"operator-bob"});
    crate::registry::write_meta(&fixture.0, &meta).unwrap();
    assert!(VmRun::read(&fixture.0).unsuccessful());
}

#[test]
fn vm_owner_backend_canonical_journal_bytes_and_additive_field_tolerance() {
    let j = journal();
    assert_eq!(format!("{}\n", serde_json::to_string(&j).unwrap()), GOLDEN);
    let mut value: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    value["futureField"] = serde_json::json!({"ignored":true});
    let j: Journal = serde_json::from_value(value).unwrap();
    assert_eq!(
        judge(j.unit.as_ref().unwrap(), j.clone(), false).verdict,
        Verdict::Verified
    );
}

#[test]
fn vm_owner_backend_attach_restart_reconstructs_without_fleet_or_transport() {
    let f = Fixture::new();
    f.write(&journal(), false);
    let mut first = f.observe();
    assert_eq!(first.liveness(), RunLiveness::Finished);
    assert_eq!(first.tailer.hosts.len(), 1);
    assert_eq!(first.tailer.vm.summary.verified, 2);
    drop(first);
    let mut attached = f.observe();
    assert_eq!(attached.liveness(), RunLiveness::Finished);
    assert_eq!(
        attached.tailer.vm.plan.as_ref().unwrap().requested_guests,
        ["guest-a"]
    );
    assert_eq!(
        attached.tailer.vm.owners["host-a"]
            .journal
            .as_ref()
            .unwrap()
            .manifest
            .as_ref()
            .unwrap()
            .guests
            .len(),
        2
    );
    assert_eq!(attached.tailer.vm.summary.verified, 2);
    assert_eq!(attached.tailer.hosts["host-a"].state, HostState::Confirmed);
}

#[test]
fn vm_owner_backend_zero_rc_and_host_confirmation_cannot_mask_missing_or_failed_guest() {
    for missing in [false, true] {
        let f = Fixture::new();
        let mut j = journal();
        j.observed
            .as_mut()
            .unwrap()
            .guests
            .get_mut("guest-a")
            .unwrap()
            .ready = false;
        f.write(&j, false);
        if missing {
            std::fs::remove_file(f.0.join("vm/host-a.json")).unwrap();
        }
        let mut obs = f.observe();
        assert_eq!(obs.liveness(), RunLiveness::Failed);
        assert!(obs.tailer.has_unsuccessful_host());
        assert_eq!(obs.tailer.hosts.len(), 1);
        if !missing {
            assert_eq!(obs.tailer.vm.summary.failed, 1);
            assert_eq!(obs.tailer.vm.summary.verified, 1);
        }
    }
}

#[test]
fn vm_owner_backend_pending_preview_and_verified_rollback_remain_distinct() {
    let f = Fixture::new();
    let mut j = journal();
    j.state = State::Pending;
    j.guests
        .values_mut()
        .for_each(|g| *g = GuestOutcome::Pending);
    let previous = j.previous.clone().unwrap();
    let observed = j.observed.as_mut().unwrap();
    observed.owner_running = previous.owner_running.clone();
    observed.guests = previous.guests.clone();
    f.write(&j, false);
    let mut obs = f.observe();
    assert_eq!(obs.liveness(), RunLiveness::Finished);
    assert_eq!(obs.tailer.vm.summary.pending, 2);
    assert_eq!(obs.tailer.vm.summary.verified, 0);
    j.state = State::Prepared;
    j.observed = None;
    j.guests
        .values_mut()
        .for_each(|g| *g = GuestOutcome::Preview);
    f.write(&j, true);
    let mut obs = f.observe();
    assert_eq!(obs.liveness(), RunLiveness::Finished);
    assert_eq!(obs.tailer.vm.summary.preview, 2);
    assert_eq!(obs.tailer.vm.summary.verified, 0);
    j.state = State::Restored;
    j.observed = Some(previous);
    j.guests
        .values_mut()
        .for_each(|g| *g = GuestOutcome::Restored);
    f.write(&j, false);
    let mut obs = f.observe();
    assert_eq!(obs.liveness(), RunLiveness::RolledBack);
    assert_eq!(obs.tailer.vm.summary.restored, 2);
}

#[test]
fn vm_owner_backend_host_only_rollback_and_partial_recovery_never_become_rolled_back() {
    let f = Fixture::new();
    let mut j = journal();
    j.state = State::Restored;
    j.guests
        .values_mut()
        .for_each(|g| *g = GuestOutcome::Restored);
    let previous = j.previous.clone().unwrap();
    let observed = j.observed.as_mut().unwrap();
    observed.owner_profile = previous.owner_profile;
    observed.owner_running = previous.owner_running;
    observed.boot_default = previous.boot_default;
    observed
        .guests
        .insert("guest-a".into(), previous.guests["guest-a"].clone());
    j.primary_failure = Some("live readiness failed".into());
    j.recovery_failure = Some("guest-b still running desired image".into());
    f.write(&j, false);
    let mut obs = f.observe();
    assert_eq!(obs.liveness(), RunLiveness::Failed);
    let view = &obs.tailer.vm.owners["host-a"];
    assert_eq!(view.verdict, Verdict::PartialRecovery);
    assert_eq!(view.guests["guest-a"], GuestOutcome::Restored);
    assert_eq!(view.guests["guest-b"], GuestOutcome::Failed);
    assert!(view.journal.as_ref().unwrap().primary_failure.is_some());
    assert!(view.journal.as_ref().unwrap().recovery_failure.is_some());
}

#[test]
fn vm_owner_backend_events_remain_owner_attributed_and_partial_lines_tolerated() {
    let f = Fixture::new();
    f.write(&journal(), false);
    let mut obs = f.observe();
    obs.poll();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(f.0.join("host-a.jsonl"))
        .unwrap();
    file.write_all(b"{\"v\":2,\"host\":\"host-a\",\"plugin\":\"deploy\",\"event\":\"vm\",\"owner\":\"host-a\",\"guest\":\"guest-b\",\"outcome\":\"confirmed\",\"future\":true}").unwrap();
    assert_eq!(obs.poll(), 0);
    file.write_all(b"\n").unwrap();
    assert_eq!(obs.poll(), 1);
    assert_eq!(obs.tailer.hosts.len(), 1);
    assert_eq!(obs.tailer.vm.summary.verified, 2);
}

#[test]
fn vm_owner_backend_unknown_versions_and_interruption_fail_closed_legacy_absence_does_not() {
    let f = Fixture::new();
    let mut j = journal();
    j.version = 99;
    f.write(&j, false);
    assert_eq!(f.observe().liveness(), RunLiveness::Failed);
    j.version = 1;
    j.state = State::Mutating;
    f.write(&j, false);
    assert_eq!(f.observe().liveness(), RunLiveness::Failed);
    crate::registry::write_meta(&f.0, &Meta::new()).unwrap();
    assert!(VmRun::read(&f.0).has_failure());
    std::fs::remove_dir_all(f.0.join("vm")).unwrap();
    assert!(!VmRun::read(&f.0).present());
}

#[test]
fn vm_owner_resolution_manifest_errors_name_schema_and_relation_fields() {
    let (mut m, unit, closure) = crate::vm::tests::manifest();
    m.guests
        .insert("unrelated".into(), m.guests["guest-a"].clone());
    assert_eq!(m.validate(&unit, &closure).unwrap_err().field, "guests");
    let (mut m, unit, closure) = crate::vm::tests::manifest();
    m.guests.get_mut("guest-a").unwrap().system = "aarch64-linux".into();
    assert_eq!(
        m.validate(&unit, &closure).unwrap_err().field,
        "guests.guest-a.system"
    );
    let mut value = serde_json::to_value(journal().manifest.unwrap()).unwrap();
    value["guests"]["guest-a"]["readiness"]["deadlineSeconds"] = serde_json::json!("unbounded");
    assert_eq!(
        super::super::manifest::Manifest::parse(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .field,
        "guests.guest-a.readiness.deadlineSeconds"
    );
}
