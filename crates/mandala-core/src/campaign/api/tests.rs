use super::*;
use crate::campaign::tests::{now, target};
use std::sync::{
    Arc, Barrier,
    atomic::{AtomicU64, Ordering},
};

struct Fixture {
    root: PathBuf,
    store: Store,
    id: String,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "mandala-campaign-api-{}-{}",
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
    fn snapshots(&self) {
        for name in ["node-a", "node-b", "node-c"] {
            self.snapshot(name,json!({"current":"/nix/store/system-a","booted":"/nix/store/system-a","system_profile":"/nix/store/system-a","captured_at":Utc::now().to_rfc3339()}));
        }
    }
    fn snapshot(&self, name: &str, value: Value) {
        std::fs::write(
            self.root.join(format!("{name}.json")),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }
    fn bytes(&self) -> Vec<u8> {
        std::fs::read(self.store.path(&self.id).unwrap().join(MANIFEST)).unwrap()
    }
    fn survey_ok() -> io::Result<Survey> {
        Ok(Survey {
            rc: 0,
            stdout: "survey fixture".into(),
            stderr: String::new(),
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn list_and_inspect_are_read_only_and_do_not_create_missing_roots() {
    let root =
        std::env::temp_dir().join(format!("mandala-missing-campaigns-{}", std::process::id()));
    let store = Store::new(root.join("campaigns"));
    assert_eq!(inspect(&store, None, 0, 50, true).unwrap()["total"], 0);
    assert!(!root.exists());
}

#[test]
fn detail_pages_targets_attempts_sources_and_verification_with_totals() {
    let f = Fixture::new();
    f.snapshots();
    verify_with_survey(&f.store, &f.id, true, |_, _| Fixture::survey_ok()).unwrap();
    let page = inspect(&f.store, Some(&f.id), 0, 1, false).unwrap();
    assert_eq!(page["members"].as_array().unwrap().len(), 1);
    assert_eq!(page["original_count"], 3);
    assert_eq!(page["eligible_count"], 3);
    assert_eq!(page["next_offset"], 1);
    assert_eq!(page["members"][0]["name"], "node-a");
    assert_eq!(page["source"]["inputs_total"], 1);
    assert_eq!(page["source"]["inputs"].as_object().unwrap().len(), 1);
    assert_eq!(page["verification"]["results_total"], 3);
    assert_eq!(
        page["verification"]["results"].as_object().unwrap().len(),
        1
    );
    assert_eq!(page["verification"]["results_next_offset"], 1);
    assert_eq!(page["attempts_total"], 1);
    let page = inspect(&f.store, Some(&f.id), 1, 1, false).unwrap();
    assert_eq!(page["members"][0]["name"], "node-b");
}

#[test]
fn bound_drift_survives_missing_checkout_and_unproven_latest_cache_is_unknown() {
    let f = Fixture::new();
    f.snapshots();
    let result = drift(&f.store, &f.id, "/missing-checkout", false).unwrap();
    assert_eq!(result["campaign"]["basis"], "bound-campaign");
    assert_eq!(result["campaign"]["complete"], true);
    assert_eq!(result["latest_source"]["expected_source"], "none");
    assert!(
        result["latest_source"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["status"] == "expectation-unknown")
    );
    let c = f.store.load(&f.id).unwrap();
    let cached: crate::drift::Toplevels = c
        .target
        .hosts
        .keys()
        .map(|name| (name.clone(), "/nix/store/new-system".into()))
        .collect::<BTreeMap<_, _>>()
        .into();
    let snapshots = crate::drift::read_snapshots(&f.root);
    let unproven = views::dual_drift(&c, &snapshots, Some(&cached), None, Utc::now()).unwrap();
    assert_eq!(
        unproven["latest_source"]["entries"][0]["status"],
        "expectation-unknown"
    );
    let proven = views::dual_drift(
        &c,
        &snapshots,
        Some(&cached),
        Some(&c.target.source),
        Utc::now(),
    )
    .unwrap();
    assert_eq!(proven["latest_source"]["entries"][0]["status"], "drift");
    assert_eq!(proven["campaign"]["complete"], true);
}

#[test]
fn open_verification_registers_before_survey_and_records_generation_bound_evidence() {
    let f = Fixture::new();
    f.snapshots();
    let result = verify_with_survey(&f.store, &f.id, true, |c, names| {
        assert_eq!(names.len(), 3);
        let persisted = f.store.load(&f.id).unwrap();
        assert_eq!(persisted.attempts[0].state, AttemptState::Running);
        assert_eq!(persisted.attempts[0].kind, AttemptKind::Verify);
        assert_eq!(c.generation, 1);
        let run = persisted.attempts[0].run_id.as_ref().unwrap();
        assert_eq!(
            registry::read_meta(&f.root.join("runs").join(run))["kind"],
            "verify"
        );
        Fixture::survey_ok()
    })
    .unwrap();
    assert_eq!(result["complete"], true);
    assert_eq!(result["recorded"], true);
    let c = f.store.load(&f.id).unwrap();
    let verification = &c.verification[0];
    assert_eq!(verification.generation, 1);
    assert_eq!(verification.attempt, 1);
    assert_eq!(verification.run_id, result["run_id"].as_str().unwrap());
    assert_eq!(c.attempts[0].state, AttemptState::Settled);
    let path = f.root.join("runs").join(&verification.run_id);
    assert!(path.join("verification.json").is_file());
    let meta = registry::read_meta(&path);
    assert_eq!(meta["rc"], 0);
    assert!(meta["pid"].is_null());
    let _runs = registry::test_hooks::install_runs_base(f.root.join("runs"));
    assert_eq!(
        registry::open_run(&verification.run_id).unwrap().liveness(),
        registry::RunLiveness::Finished
    );
    assert!(
        c.outcomes.is_empty(),
        "observation cannot invent real activation outcomes"
    );
}

#[test]
fn close_racing_survey_preserves_closed_manifest_and_observable_ordinary_run() {
    let f = Fixture::new();
    f.snapshots();
    let barrier = Arc::new(Barrier::new(2));
    let worker_barrier = Arc::clone(&barrier);
    let store = f.store.clone();
    let id = f.id.clone();
    let thread = std::thread::spawn(move || {
        verify_with_survey(&store, &id, true, |_, _| {
            worker_barrier.wait();
            worker_barrier.wait();
            Fixture::survey_ok()
        })
    });
    barrier.wait();
    let c = f.store.load(&f.id).unwrap();
    assert_eq!(c.attempts[0].state, AttemptState::Running);
    f.store
        .transaction(&f.id, |c| {
            c.close(
                c.generation,
                Closure {
                    closed_at: Utc::now(),
                    actor: "operator-alice".into(),
                    reason: "administrative close during read-only survey".into(),
                },
            )
        })
        .unwrap();
    let closed = f.bytes();
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(f.store.path(&f.id).unwrap().join(MANIFEST))
            .unwrap()
            .ino()
    };
    f.store.retain(
        &f.root.join("runs"),
        0,
        0,
        Utc::now() + chrono::TimeDelta::days(90),
    );
    assert!(
        f.store.path(&f.id).unwrap().exists(),
        "a live linked verify run protects the closed retention unit"
    );
    barrier.wait();
    let report = thread.join().unwrap().unwrap();
    assert_eq!(report["recorded"], false);
    assert_eq!(report["captured_complete"], true);
    assert_eq!(report["complete"], false);
    assert_eq!(f.bytes(), closed);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(f.store.path(&f.id).unwrap().join(MANIFEST))
                .unwrap()
                .ino(),
            inode,
            "even an identical rewrite would replace the manifest inode"
        );
    }
    let path = f.root.join("runs").join(report["run_id"].as_str().unwrap());
    let retained: Value =
        serde_json::from_slice(&std::fs::read(path.join("verification.json")).unwrap()).unwrap();
    assert_eq!(retained, report);
    assert!(registry::read_meta(&path)["pid"].is_null());
    assert!(f.store.load(&f.id).unwrap().verification.is_empty());
}

#[test]
fn closed_verification_is_comparison_only_without_survey_attempt_or_manifest_write() {
    let f = Fixture::new();
    f.snapshots();
    close(&f.store, &f.id, 0, "operator-alice", "closed", Some(&f.id)).unwrap();
    let before = f.bytes();
    let report = verify_with_survey(&f.store, &f.id, true, |_, _| {
        panic!("closed campaigns cannot start a survey")
    })
    .unwrap();
    assert_eq!(report["closed_read_only"], true);
    assert_eq!(report["recorded"], false);
    assert_eq!(report["comparison_complete"], true);
    assert_eq!(report["complete"], false);
    assert!(!f.root.join("runs").exists());
    assert_eq!(f.bytes(), before);
}

#[test]
fn verification_tracks_the_survey_child_pid_until_it_is_reaped() {
    let f = Fixture::new();
    f.snapshots();
    let store = f.store.clone();
    let id = f.id.clone();
    let root = f.root.clone();
    let thread = std::thread::spawn(move || {
        verify_registered(&store, &id, true, |_, _, run| {
            let mut command = std::process::Command::new("sh");
            command.args(["-c",r#"printf '%s\n' "$$" > "$1/survey.pid"; while [ ! -f "$1/release" ]; do sleep 0.01; done; printf 'survey done\n'"#,"survey-fixture"]).arg(&root).current_dir(&root);
            survey_command(&store, Some(run), &mut command)
        })
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let tracked = loop {
        let pid = std::fs::read_to_string(f.root.join("survey.pid"))
            .ok()
            .and_then(|pid| pid.trim().parse::<u64>().ok());
        let c = f.store.load(&f.id).unwrap();
        let meta = c
            .attempts
            .first()
            .and_then(|attempt| attempt.run_id.as_ref())
            .map(|run| registry::read_meta(&f.root.join("runs").join(run)));
        if let (Some(pid), Some(meta)) = (pid, meta)
            && meta.get("pid").and_then(Value::as_u64) == Some(pid)
        {
            break pid != u64::from(std::process::id());
        }
        if std::time::Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    std::fs::write(f.root.join("release"), "").unwrap();
    let report = thread.join().unwrap().unwrap();
    assert!(
        tracked,
        "the live survey, not a long-lived frontend, must own registry liveness"
    );
    let meta = registry::read_meta(&f.root.join("runs").join(report["run_id"].as_str().unwrap()));
    assert!(meta["pid"].is_null());
    assert_eq!(meta["controller_pid"], std::process::id());
    assert_eq!(report["complete"], true);
}

#[test]
fn policy_change_during_survey_never_asserts_completion_for_new_denominators() {
    let f = Fixture::new();
    f.snapshots();
    let report = verify_with_survey(&f.store, &f.id, true, |c, _| {
        exclude(
            &f.store,
            &f.id,
            PolicyRequest {
                generation: c.generation,
                actor: "operator-alice",
                reason: "maintenance",
                confirm: Some(&f.id),
            },
            BTreeMap::from([("node-b".into(), "maintenance".into())]),
            Vec::new(),
        )
        .unwrap();
        Fixture::survey_ok()
    })
    .unwrap();
    assert_eq!(report["captured_complete"], true);
    assert_eq!(report["complete"], false);
    assert_eq!(report["generation_stale"], true);
    let c = f.store.load(&f.id).unwrap();
    assert_eq!(c.verification[0].generation, 1);
    assert_eq!(c.generation, 2);
    assert!(!c.verification[0].complete);
    assert_eq!(c.verification[0].results.len(), 3);
    assert_eq!(
        c.eligible(AttemptKind::Verify, None).unwrap(),
        ["node-a", "node-c"]
    );
}

#[test]
fn missing_boot_and_staged_profile_are_incomplete_but_exclusions_are_explicit() {
    let f = Fixture::new();
    f.snapshots();
    f.snapshot("node-b",json!({"current":"/nix/store/system-a","system_profile":"/nix/store/system-a","captured_at":Utc::now().to_rfc3339()}));
    let report = verify_with_survey(&f.store, &f.id, true, |_, _| Fixture::survey_ok()).unwrap();
    assert_eq!(report["complete"], false);
    f.snapshot("node-b",json!({"current":"/nix/store/old","booted":"/nix/store/old","system_profile":"/nix/store/system-a","captured_at":Utc::now().to_rfc3339()}));
    let report = verify_with_survey(&f.store, &f.id, true, |_, _| Fixture::survey_ok()).unwrap();
    assert!(
        report["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["host"] == "node-b" && entry["status"] == "reboot-pending")
    );
    assert_eq!(report["complete"], false);
    let gen0 = f.store.load(&f.id).unwrap().generation;
    exclude(
        &f.store,
        &f.id,
        PolicyRequest {
            generation: gen0,
            actor: "operator-alice",
            reason: "maintenance",
            confirm: Some(&f.id),
        },
        BTreeMap::from([("node-b".into(), "maintenance".into())]),
        Vec::new(),
    )
    .unwrap();
    let report = verify_with_survey(&f.store, &f.id, true, |_, _| Fixture::survey_ok()).unwrap();
    assert_eq!(report["complete"], true);
    assert_eq!(report["original_count"], 3);
    assert_eq!(report["eligible_count"], 2);
    assert_eq!(report["exclusions"]["node-b"]["reason"], "maintenance");
}

#[test]
fn policy_refusals_leave_bytes_unchanged_and_narrowing_uses_bound_members() {
    let f = Fixture::new();
    let before = f.bytes();
    assert_eq!(
        exclude(
            &f.store,
            &f.id,
            PolicyRequest {
                generation: 0,
                actor: "operator-alice",
                reason: "maintenance",
                confirm: None
            },
            BTreeMap::new(),
            vec!["node-b".into()]
        )
        .unwrap()["refused"],
        true
    );
    assert_eq!(
        close(&f.store, &f.id, 0, "operator-alice", "closed", None).unwrap()["refused"],
        true
    );
    assert_eq!(f.bytes(), before);
    assert_eq!(
        narrow(&f.store, &f.id, Some("all,!node-b"))
            .unwrap()
            .unwrap(),
        BTreeSet::from(["node-a".into(), "node-c".into()])
    );
    assert!(
        narrow(&f.store, &f.id, Some("!all"))
            .unwrap()
            .unwrap()
            .is_empty()
    );
    assert!(narrow(&f.store, &f.id, Some("new-node")).is_err());
    assert_eq!(
        close(&f.store, &f.id, 99, "operator-alice", "stale", Some(&f.id))
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(f.bytes(), before);
}

#[test]
fn reboot_selection_uses_fresh_installed_proof_and_drift_never_grants_retry_permission() {
    let f = Fixture::new();
    f.snapshots();
    f.store
        .transaction(&f.id, |c| {
            let source = c.target.source.clone();
            c.prepare(Dispatch {
                kind: AttemptKind::Deploy,
                generation: 0,
                source: Some(&source),
                narrow: None,
                dry_activate: false,
                confirm: Some("node-a,node-b,node-c"),
            })?;
            c.link_run(1, "20260102T030406_000001-123")?;
            c.settle(
                1,
                ["node-a", "node-b", "node-c"]
                    .into_iter()
                    .map(|name| (name.into(), HostOutcome::RebootPending))
                    .collect(),
            )
        })
        .unwrap();
    f.snapshot("node-b",json!({"current":"/nix/store/old","booted":"/nix/store/old","system_profile":"/nix/store/system-a","captured_at":Utc::now().to_rfc3339()}));
    f.snapshot("node-c",json!({"current":"/nix/store/old","booted":"/nix/store/old","captured_at":Utc::now().to_rfc3339()}));
    let c = f.store.load(&f.id).unwrap();
    let (names, blocked) =
        views::reboot_candidates(&c, &crate::drift::read_snapshots(&f.root), None, Utc::now())
            .unwrap();
    assert_eq!(names, BTreeSet::from(["node-b".into()]));
    assert!(blocked.contains_key("node-c"));
    assert!(!blocked.contains_key("node-a"));
    f.snapshot("node-a",json!({"current":"/nix/store/drifted","booted":"/nix/store/drifted","system_profile":"/nix/store/drifted","captured_at":Utc::now().to_rfc3339()}));
    assert!(views::entries(&c,&crate::drift::read_snapshots(&f.root),Utc::now()).unwrap().iter().any(|entry|entry.host=="node-a"&&entry.status==crate::drift::DriftStatus::Drift));
    assert!(
        c.eligible(AttemptKind::Deploy, None).unwrap().is_empty(),
        "observation cannot downgrade successful staging into retry permission"
    );
}

#[test]
fn legacy_cache_forms_are_never_campaign_identity_and_future_schema_has_no_clean_judgment() {
    let f = Fixture::new();
    f.snapshots();
    for cache in [
        "not-json",
        "{}",
        r#"{"rev":"revision-a","toplevels":{"node-a":"/nix/store/system-a"}}"#,
        r#"{"rev":"revision-a-dirty","toplevels":{}}"#,
    ] {
        std::fs::write(f.root.join(".expected.json"), cache).unwrap();
        let result = drift(&f.store, &f.id, "/missing-source", false).unwrap();
        assert_eq!(result["latest_source"]["expected_source"], "none");
        assert_eq!(
            result["latest_source"]["entries"][0]["status"],
            "expectation-unknown"
        );
        assert_eq!(result["campaign"]["complete"], true);
    }
    let path = f.store.path(&f.id).unwrap().join(MANIFEST);
    let mut value: Value = serde_json::from_slice(&f.bytes()).unwrap();
    value["schema_version"] = json!(99);
    std::fs::write(path, canonical(&value).unwrap()).unwrap();
    assert_eq!(
        inspect(&f.store, Some(&f.id), 0, 50, true).unwrap()["unsupported"],
        true
    );
    assert!(drift(&f.store, &f.id, "/missing-source", false).is_err());
}

#[test]
fn missing_expectations_and_incomplete_entry_sets_never_mean_complete() {
    let f = Fixture::new();
    f.snapshots();
    let c = f.store.load(&f.id).unwrap();
    let entries = views::entries(&c, &crate::drift::read_snapshots(&f.root), Utc::now()).unwrap();
    assert!(views::complete(&c, &entries));
    assert!(!views::complete(&c, &entries[..1]));
    assert!(!views::complete(&c, &vec![entries[0].clone(); 3]));
    let entries = crate::drift::compare(
        &c.target.hosts.keys().cloned().collect::<Vec<_>>(),
        &crate::drift::read_snapshots(&f.root),
        None,
        Some(crate::drift::default_max_age()),
        Utc::now(),
    );
    assert!(
        entries
            .iter()
            .all(|entry| entry.status == crate::drift::DriftStatus::ExpectationUnknown)
    );
}
