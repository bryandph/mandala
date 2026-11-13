use super::*;
use crate::campaign::tests::{now, target};
use crate::runner::EventWriter;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture {
    root: PathBuf,
    store: Store,
    id: String,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "mandala-campaign-execution-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let store = Store::new(root.join("campaigns"));
        let c = store.create(target(), now()).unwrap();
        Self {
            root,
            store,
            id: c.campaign_id,
        }
    }
    fn prepare(&self) {
        self.store
            .transaction(&self.id, |c| {
                let source = c.target.source.clone();
                c.prepare(Dispatch {
                    kind: AttemptKind::Deploy,
                    generation: 0,
                    source: Some(&source),
                    narrow: None,
                    dry_activate: false,
                    confirm: Some("node-a,node-b,node-c"),
                })?;
                Ok(())
            })
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn dead_unlinked_preparation_reconciles_to_pending_without_a_new_run() {
    let f = Fixture::new();
    f.prepare();
    let _pids = registry::test_hooks::install(|_| false);
    reconcile(&f.store, &f.id).unwrap();
    let c = f.store.load(&f.id).unwrap();
    assert_eq!(c.attempts[0].state, AttemptState::Settled);
    assert!(c.outcomes.is_empty());
    assert_eq!(c.eligible(AttemptKind::Deploy, None).unwrap().len(), 3);
    assert!(!f.root.join("runs").exists());
}

#[test]
fn live_prepared_owner_cannot_be_reconciled_or_replayed() {
    let f = Fixture::new();
    f.prepare();
    let owner = i64::from(std::process::id());
    let _pids = registry::test_hooks::install(move |pid| pid == Some(owner));
    assert_eq!(
        reconcile(&f.store, &f.id).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(
        f.store.load(&f.id).unwrap().attempts[0].state,
        AttemptState::Prepared
    );
}

#[test]
fn linked_reconciliation_uses_terminal_host_evidence_and_preserves_run_failure() {
    let f = Fixture::new();
    f.prepare();
    let _runs = registry::test_hooks::install_runs_base(f.root.join("runs"));
    let _pids = registry::test_hooks::install(|_| false);
    let (run_id, path) = registry::new_run_dir().unwrap();
    registry::write_meta(
        &path,
        serde_json::json!({"kind":"deploy","pid":null,"rc":1})
            .as_object()
            .unwrap(),
    )
    .unwrap();
    for (name, milestone) in [
        ("node-a", "confirm"),
        ("node-b", "failed"),
        ("node-c", "reboot-pending"),
    ] {
        let writer = EventWriter::new(&path, name, name, "deploy").unwrap();
        if milestone == "failed" {
            writer
                .emit(
                    "status",
                    serde_json::json!({"state":"done","rc":1})
                        .as_object()
                        .unwrap()
                        .clone(),
                )
                .unwrap();
        } else {
            writer
                .emit(
                    "milestone",
                    serde_json::json!({"milestone":milestone})
                        .as_object()
                        .unwrap()
                        .clone(),
                )
                .unwrap();
        }
    }
    f.store
        .transaction(&f.id, |c| c.link_run(1, &run_id))
        .unwrap();
    reconcile(&f.store, &f.id).unwrap();
    let c = f.store.load(&f.id).unwrap();
    assert_eq!(c.eligible(AttemptKind::Deploy, None).unwrap(), ["node-b"]);
    assert_eq!(c.eligible(AttemptKind::Reboot, None).unwrap(), ["node-c"]);
    assert_eq!(registry::read_meta(&path)["rc"], 1);
    assert_eq!(c.outcomes["node-a"].run_id, run_id);
    assert_eq!(c.attempts.len(), 1);
}

#[test]
fn a_controller_exit_without_complete_host_evidence_remains_uncertain() {
    let f = Fixture::new();
    f.prepare();
    let _runs = registry::test_hooks::install_runs_base(f.root.join("runs"));
    let _pids = registry::test_hooks::install(|_| false);
    let (run_id, path) = registry::new_run_dir().unwrap();
    registry::write_meta(
        &path,
        serde_json::json!({"pid":null,"rc":0}).as_object().unwrap(),
    )
    .unwrap();
    f.store
        .transaction(&f.id, |c| c.link_run(1, &run_id))
        .unwrap();
    assert!(reconcile(&f.store, &f.id).is_err());
    assert_eq!(
        f.store.load(&f.id).unwrap().attempts[0].state,
        AttemptState::Running
    );
}

#[test]
fn additive_run_metadata_pins_attempt_digest_generation_and_identity() {
    let f = Fixture::new();
    f.prepare();
    let c = f.store.load(&f.id).unwrap();
    let metadata = metadata(&c, 1).unwrap();
    assert_eq!(metadata["campaign_id"], f.id);
    assert_eq!(metadata["campaign_target_digest"], c.target_digest);
    assert_eq!(metadata["campaign_generation"], 1);
    assert_eq!(metadata["campaign_attempt"], 1);
    assert!(!metadata.contains_key("rc"));
    assert!(!metadata.contains_key("targets"));
}

#[test]
fn reconstruction_uses_captured_activation_and_settings_not_latest_inventory() {
    let f = Fixture::new();
    let mut c = f.store.load(&f.id).unwrap();
    c.target.hosts.get_mut("node-a").unwrap().activation = ActivationMode::Boot;
    let request = DeployRequest {
        generation: 0,
        confirm: None,
        narrow: None,
        dry_activate: true,
        throttle: 2,
        halt_on_build_failure: false,
        publish: Default::default(),
    };
    let p = plan(&c, &["node-a".into()], &request).unwrap();
    assert_eq!(p.settings["node-a"]["activation"], "boot");
    assert_eq!(p.settings["node-a"]["hostname"], "node-a");
    assert_eq!(p.targets, ["node-a"]);
    assert!(p.dry_activate);
}

#[test]
fn failed_root_initialization_never_publishes_a_ready_manifest() {
    let f = Fixture::new();
    assert!(
        f.store
            .create_with(target(), now(), |_| Err(invalid("injected root failure")))
            .is_err()
    );
    let entries = std::fs::read_dir(&f.store.root).unwrap();
    let manifests = entries
        .flatten()
        .filter(|entry| entry.path().join(MANIFEST).exists())
        .count();
    assert_eq!(
        manifests, 1,
        "only the original complete fixture may be ready"
    );
}
