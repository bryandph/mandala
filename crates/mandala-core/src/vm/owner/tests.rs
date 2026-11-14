use super::*;
use crate::vm::manifest::Runtime;
use crate::vm::tests::{manifest, path};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{BufReader, DuplexStream, ReadHalf, WriteHalf};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mandala-vm-owner-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn journal(&self, owner: &str) -> Journal {
        serde_json::from_slice(&std::fs::read(self.0.join(owner).join("journal.json")).unwrap())
            .unwrap()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct MockState {
    trace: Vec<String>,
    fail_root: bool,
    mismatch: bool,
    mismatch_identity: bool,
    wrong_inspect_evidence: bool,
    partial_restore: bool,
    restored: bool,
    staged: bool,
    block_apply: bool,
    missing_previous: bool,
    restore_failure: bool,
}
struct Mock {
    state: Mutex<MockState>,
    root: PathBuf,
    barrier: tokio::sync::Notify,
}
fn previous(manifest: &Manifest) -> Observation {
    Observation {
        manifest_version: 1,
        adapter: manifest.adapter.clone(),
        owner_profile: path("old-owner"),
        owner_running: path("old-running"),
        boot_default: path("old-boot"),
        guests: manifest
            .guests
            .iter()
            .map(|(name, guest)| {
                (
                    name.clone(),
                    Runtime {
                        running_identity: path(&format!("old-{name}")),
                        artifact: path(&format!("old-{name}")),
                        runtime_instance: guest.runtime_instance.clone(),
                        readiness: guest.readiness.clone(),
                        recovery_contract: guest.recovery_contract.clone(),
                        ready: true,
                        diagnostic: String::new(),
                    },
                )
            })
            .collect(),
    }
}
impl Effects for Arc<Mock> {
    async fn adapter(&self, request: AdapterRequest) -> io::Result<AdapterReply> {
        if matches!(request.operation, Operation::Apply) && self.state.lock().unwrap().block_apply {
            self.barrier.notify_one();
            std::future::pending::<()>().await;
        }
        let mut state = self.state.lock().unwrap();
        state.trace.push(format!("{:?}", request.operation));
        let mut observation = previous(&request.manifest);
        let evidence = match request.operation {
            Operation::Inspect => {
                if state.wrong_inspect_evidence {
                    return Ok(AdapterReply {
                        version: VERSION,
                        evidence: AdapterEvidence::Verify { observation },
                    });
                }
                if state.missing_previous {
                    observation.guests.clear();
                }
                AdapterEvidence::Inspect { observation }
            }
            Operation::Prepare => AdapterEvidence::Prepare {
                supported_systems: request.manifest.supported_systems.clone(),
                runtime_instances: request
                    .manifest
                    .guests
                    .iter()
                    .map(|(n, g)| (n.clone(), g.runtime_instance.clone()))
                    .collect(),
                available_paths: request
                    .manifest
                    .guests
                    .values()
                    .map(|g| g.artifact.clone())
                    .collect(),
            },
            Operation::Apply => {
                let journal: Journal =
                    serde_json::from_slice(&std::fs::read(self.root.join("host-a/journal.json"))?)
                        .unwrap();
                assert_eq!(
                    journal.state,
                    State::Mutating,
                    "durable mutation intent precedes adapter effects"
                );
                assert!(journal.previous.is_some());
                assert!(state.trace.iter().any(|s| s.starts_with("root:")));
                state.staged = request.mode == Mode::Boot;
                AdapterEvidence::Apply {
                    dispositions: request
                        .manifest
                        .guests
                        .keys()
                        .map(|n| {
                            (
                                n.clone(),
                                if state.staged {
                                    GuestOutcome::Pending
                                } else {
                                    GuestOutcome::Confirmed
                                },
                            )
                        })
                        .collect(),
                }
            }
            Operation::Verify => {
                if request.target == VerificationTarget::Desired {
                    observation.owner_profile = request.manifest.owner_generation.clone();
                    observation.boot_default = request.manifest.owner_generation.clone();
                    if request.mode == Mode::Live {
                        observation.owner_running = request.manifest.owner_generation.clone();
                        for (name, runtime) in &mut observation.guests {
                            runtime.running_identity =
                                request.manifest.guests[name].desired_identity.clone();
                            runtime.artifact = request.manifest.guests[name].artifact.clone();
                        }
                    }
                    if state.mismatch {
                        observation.guests.get_mut("guest-a").unwrap().ready = false;
                    }
                    if state.mismatch_identity {
                        observation
                            .guests
                            .get_mut("guest-a")
                            .unwrap()
                            .running_identity = path("wrong-running-generation");
                    }
                } else {
                    assert!(state.restored);
                    if state.partial_restore {
                        observation
                            .guests
                            .get_mut("guest-b")
                            .unwrap()
                            .running_identity = "wrong-image".into();
                    }
                }
                AdapterEvidence::Verify { observation }
            }
            Operation::Restore => {
                if state.restore_failure {
                    return Err(invalid("stub restore failed"));
                }
                state.restored = true;
                AdapterEvidence::Restore { observation }
            }
        };
        Ok(AdapterReply {
            version: VERSION,
            evidence,
        })
    }
    async fn closure(&self, _: &str, _: &Manifest) -> io::Result<BTreeSet<String>> {
        self.state
            .lock()
            .unwrap()
            .trace
            .push("closure-query".into());
        Ok(manifest().2)
    }
    async fn root(&self, path: &str, _: &Path) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        state.trace.push(format!("root:{path}"));
        if state.fail_root {
            Err(invalid("stub rooting failure"))
        } else {
            Ok(())
        }
    }
}

struct Client {
    input: BufReader<ReadHalf<DuplexStream>>,
    output: WriteHalf<DuplexStream>,
    server: tokio::task::JoinHandle<io::Result<()>>,
}
impl Client {
    async fn new(root: &Temp, effects: Arc<Mock>) -> Self {
        Self::new_for_owner(root, effects, "host-a").await
    }
    async fn new_for_owner(root: &Temp, effects: Arc<Mock>, expected_owner: &'static str) -> Self {
        let (client, server) = tokio::io::duplex(65536);
        let root = root.0.clone();
        let (input, output) = tokio::io::split(client);
        let task = tokio::spawn(async move {
            let (input, mut output) = tokio::io::split(server);
            serve(
                &mut BufReader::new(input),
                &mut output,
                &root,
                expected_owner,
                &effects,
                Duration::from_millis(40),
            )
            .await
        });
        Self {
            input: BufReader::new(input),
            output,
            server: task,
        }
    }
    async fn request(&mut self, request: Request) -> Reply {
        let mut bytes = serde_json::to_vec(&request).unwrap();
        bytes.push(b'\n');
        self.output.write_all(&bytes).await.unwrap();
        self.output.flush().await.unwrap();
        serde_json::from_str(&line(&mut self.input).await.unwrap().expect("reply")).unwrap()
    }
    async fn hello(&mut self) {
        self.request(Request::Hello { version: VERSION })
            .await
            .check()
            .unwrap();
    }
    async fn lock(&mut self, owner: &str, transaction: &str, dry: bool, reconcile: bool) -> Reply {
        self.request(Request::Lock {
            owner: owner.into(),
            transaction: transaction.into(),
            dry,
            reconcile,
        })
        .await
    }
    async fn prepare(&mut self) -> Reply {
        let (manifest, unit, _) = manifest();
        self.request(Request::Prepare {
            manifest: Box::new(manifest),
            unit: Box::new(unit),
            profile: path("profile"),
        })
        .await
    }
    async fn close(self) -> io::Result<()> {
        drop(self.output);
        drop(self.input);
        tokio::time::timeout(Duration::from_secs(2), self.server)
            .await
            .unwrap()
            .unwrap()
    }
}
fn mock(root: &Temp) -> Arc<Mock> {
    Arc::new(Mock {
        root: root.0.clone(),
        state: Mutex::new(MockState::default()),
        barrier: tokio::sync::Notify::new(),
    })
}

#[tokio::test]
async fn vm_owner_backend_protocol_mismatch_both_directions_before_effects() {
    let root = Temp::new();
    let effects = mock(&root);
    let mut client = Client::new(&root, effects.clone()).await;
    let reply = client.request(Request::Hello { version: 99 }).await;
    assert!(reply.error.unwrap().contains("controller 99, helper 1"));
    assert!(client.close().await.is_err());
    assert!(effects.state.lock().unwrap().trace.is_empty());
    let error = Reply {
        version: 99,
        ok: true,
        error: None,
        journal: None,
    }
    .check()
    .unwrap_err();
    assert!(error.to_string().contains("controller 1, helper 99"));
    assert!(!root.0.join("host-a").exists());
}

#[tokio::test]
async fn vm_owner_backend_cross_controller_lock_barrier_and_independent_owners() {
    let root = Temp::new();
    let effects = mock(&root);
    let mut first = Client::new(&root, effects.clone()).await;
    first.hello().await;
    assert!(!first.lock("host-b", "wrong-owner", false, false).await.ok);
    first
        .lock("host-a", "run-a", false, false)
        .await
        .check()
        .unwrap();
    let mut sibling = Client::new(&root, effects.clone()).await;
    sibling.hello().await;
    assert!(
        sibling
            .lock("host-a", "run-b", false, false)
            .await
            .error
            .unwrap()
            .contains("busy")
    );
    let mut independent = Client::new_for_owner(&root, effects, "host-b").await;
    independent.hello().await;
    independent
        .lock("host-b", "run-c", false, false)
        .await
        .check()
        .unwrap();
    assert!(first.close().await.is_err());
    assert!(
        sibling
            .lock("host-a", "run-b", false, false)
            .await
            .error
            .unwrap()
            .contains("reconciliation")
    );
    assert!(sibling.close().await.is_err());
    assert!(independent.close().await.is_err());
}

#[tokio::test]
async fn vm_owner_backend_persists_and_roots_before_mutation_and_verifies_all_guests() {
    let root = Temp::new();
    let effects = mock(&root);
    let mut client = Client::new(&root, effects.clone()).await;
    client.hello().await;
    client
        .lock("host-a", "run-a", false, false)
        .await
        .check()
        .unwrap();
    assert!(!client.request(Request::HostStart).await.ok);
    client.prepare().await.check().unwrap();
    assert_eq!(root.journal("host-a").state, State::Prepared);
    client.request(Request::HostStart).await.check().unwrap();
    let journal = client
        .request(Request::Apply { mode: Mode::Live })
        .await
        .check()
        .unwrap()
        .unwrap();
    assert_eq!(journal.state, State::Confirmed);
    assert!(
        journal
            .guests
            .values()
            .all(|s| *s == GuestOutcome::Confirmed)
    );
    assert_eq!(
        effects
            .state
            .lock()
            .unwrap()
            .trace
            .iter()
            .filter(|s| *s == "Apply")
            .count(),
        1
    );
    client.request(Request::Finish).await.check().unwrap();
    client.close().await.unwrap();
}

#[tokio::test]
async fn vm_owner_backend_root_failure_prevents_host_and_runtime_mutation() {
    let root = Temp::new();
    let effects = mock(&root);
    effects.state.lock().unwrap().fail_root = true;
    let mut client = Client::new(&root, effects.clone()).await;
    client.hello().await;
    client
        .lock("host-a", "run-a", false, false)
        .await
        .check()
        .unwrap();
    assert!(!client.prepare().await.ok);
    assert!(!client.request(Request::HostStart).await.ok);
    assert!(!client.request(Request::Apply { mode: Mode::Live }).await.ok);
    assert!(
        !effects
            .state
            .lock()
            .unwrap()
            .trace
            .iter()
            .any(|s| s == "Apply")
    );
    assert!(client.close().await.is_err());
}

#[tokio::test]
async fn vm_owner_backend_loss_is_unknown_blocks_replay_and_explicit_restore_reconciles() {
    let root = Temp::new();
    let effects = mock(&root);
    let mut client = Client::new(&root, effects.clone()).await;
    client.hello().await;
    client
        .lock("host-a", "run-a", false, false)
        .await
        .check()
        .unwrap();
    client.prepare().await.check().unwrap();
    client.request(Request::HostStart).await.check().unwrap();
    assert!(client.close().await.is_err());
    assert_eq!(root.journal("host-a").state, State::Unknown);
    let mut next = Client::new(&root, effects.clone()).await;
    next.hello().await;
    assert!(!next.lock("host-a", "run-b", false, false).await.ok);
    next.lock("host-a", "run-b", false, true)
        .await
        .check()
        .unwrap();
    assert!(!next.request(Request::HostStart).await.ok);
    let journal = next
        .request(Request::Reconcile { restore: true })
        .await
        .check()
        .unwrap()
        .unwrap();
    assert_eq!(journal.state, State::Restored);
    assert!(
        journal
            .guests
            .values()
            .all(|s| *s == GuestOutcome::Restored)
    );
    next.request(Request::Finish).await.check().unwrap();
    next.close().await.unwrap();
    let mut admitted = Client::new(&root, effects).await;
    admitted.hello().await;
    admitted
        .lock("host-a", "run-c", true, false)
        .await
        .check()
        .unwrap();
    assert!(admitted.close().await.is_err());
}

#[tokio::test]
async fn vm_owner_backend_partial_restore_preserves_sibling_evidence_and_both_failures() {
    let root = Temp::new();
    let effects = mock(&root);
    {
        let mut state = effects.state.lock().unwrap();
        state.mismatch = true;
        state.partial_restore = true;
    }
    let mut client = Client::new(&root, effects).await;
    client.hello().await;
    client
        .lock("host-a", "run-a", false, false)
        .await
        .check()
        .unwrap();
    client.prepare().await.check().unwrap();
    client.request(Request::HostStart).await.check().unwrap();
    assert!(!client.request(Request::Apply { mode: Mode::Live }).await.ok);
    let reply = client
        .request(Request::Recover {
            auto_rollback: true,
            primary: "guest readiness failed".into(),
        })
        .await;
    assert!(!reply.ok);
    let journal = reply.journal.unwrap();
    assert_eq!(journal.state, State::PartialRecovery);
    assert_eq!(journal.guests["guest-a"], GuestOutcome::Restored);
    assert_eq!(journal.guests["guest-b"], GuestOutcome::Failed);
    assert!(journal.primary_failure.unwrap().contains("readiness"));
    assert!(journal.recovery_failure.is_some());
    client.request(Request::Finish).await.check().unwrap();
    client.close().await.unwrap();
}

#[tokio::test]
async fn vm_owner_backend_boot_is_pending_dry_has_no_owner_recovery_mutations() {
    let root = Temp::new();
    let effects = mock(&root);
    let mut dry = Client::new(&root, effects.clone()).await;
    dry.hello().await;
    dry.lock("host-a", "dry-run", true, false)
        .await
        .check()
        .unwrap();
    let journal = dry.prepare().await.check().unwrap().unwrap();
    assert!(journal.guests.values().all(|s| *s == GuestOutcome::Preview));
    assert!(!dry.request(Request::HostStart).await.ok);
    assert!(!dry.request(Request::Apply { mode: Mode::Live }).await.ok);
    assert!(!root.0.join("host-a/journal.json").exists());
    assert!(!root.0.join("host-a/roots").exists());
    dry.request(Request::Finish).await.check().unwrap();
    dry.close().await.unwrap();
    let mut boot = Client::new(&root, effects).await;
    boot.hello().await;
    boot.lock("host-a", "boot-run", false, false)
        .await
        .check()
        .unwrap();
    boot.prepare().await.check().unwrap();
    boot.request(Request::HostStart).await.check().unwrap();
    let journal = boot
        .request(Request::Apply { mode: Mode::Boot })
        .await
        .check()
        .unwrap()
        .unwrap();
    assert_eq!(journal.state, State::Pending);
    assert!(journal.guests.values().all(|s| *s == GuestOutcome::Pending));
    boot.request(Request::Finish).await.check().unwrap();
    boot.close().await.unwrap();
}

#[tokio::test]
async fn vm_owner_backend_first_creation_refused_before_roots_or_mutation() {
    let root = Temp::new();
    let effects = mock(&root);
    effects.state.lock().unwrap().missing_previous = true;
    let mut client = Client::new(&root, effects.clone()).await;
    client.hello().await;
    client
        .lock("host-a", "20261114T120000_000001-123", false, false)
        .await
        .check()
        .unwrap();
    assert!(!client.prepare().await.ok);
    assert!(!client.request(Request::HostStart).await.ok);
    let trace = effects.state.lock().unwrap().trace.clone();
    assert_eq!(trace, ["closure-query", "Inspect"]);
    client
        .request(Request::AbortUnapplied {
            diagnostic: "no previous runtime".into(),
        })
        .await
        .check()
        .unwrap();
    client.request(Request::Finish).await.check().unwrap();
    client.close().await.unwrap();
    assert_eq!(root.journal("host-a").state, State::NotApplied);
}

#[tokio::test]
async fn vm_owner_backend_restore_error_retains_primary_and_recovery_diagnostics() {
    let root = Temp::new();
    let effects = mock(&root);
    effects.state.lock().unwrap().restore_failure = true;
    let mut client = Client::new(&root, effects).await;
    client.hello().await;
    client
        .lock("host-a", "run-a", false, false)
        .await
        .check()
        .unwrap();
    client.prepare().await.check().unwrap();
    client.request(Request::HostStart).await.check().unwrap();
    let reply = client
        .request(Request::Recover {
            auto_rollback: true,
            primary: "host magic rollback alone".into(),
        })
        .await;
    assert!(!reply.ok);
    let journal = reply.journal.unwrap();
    assert_eq!(journal.state, State::Unknown);
    assert_eq!(
        journal.primary_failure.as_deref(),
        Some("host magic rollback alone")
    );
    assert!(
        journal
            .recovery_failure
            .unwrap()
            .contains("stub restore failed")
    );
    assert!(client.close().await.is_err());
}

#[tokio::test]
async fn vm_owner_backend_identity_mismatch_never_confirms_from_host_success() {
    let root = Temp::new();
    let effects = mock(&root);
    effects.state.lock().unwrap().mismatch_identity = true;
    let mut client = Client::new(&root, effects).await;
    client.hello().await;
    client
        .lock("host-a", "run-a", false, false)
        .await
        .check()
        .unwrap();
    client.prepare().await.check().unwrap();
    client.request(Request::HostStart).await.check().unwrap();
    let reply = client.request(Request::Apply { mode: Mode::Live }).await;
    assert!(!reply.ok);
    assert_eq!(
        reply.journal.unwrap().guests["guest-a"],
        GuestOutcome::Failed
    );
    let journal = client
        .request(Request::Recover {
            auto_rollback: true,
            primary: "identity mismatch".into(),
        })
        .await
        .check()
        .unwrap()
        .unwrap();
    assert_eq!(journal.state, State::Restored);
    client.request(Request::Finish).await.check().unwrap();
    client.close().await.unwrap();
}

#[tokio::test]
async fn vm_owner_backend_wrong_inspection_evidence_refuses_before_mutation() {
    let root = Temp::new();
    let effects = mock(&root);
    effects.state.lock().unwrap().wrong_inspect_evidence = true;
    let mut client = Client::new(&root, effects.clone()).await;
    client.hello().await;
    client
        .lock("host-a", "run-a", false, false)
        .await
        .check()
        .unwrap();
    assert!(!client.prepare().await.ok);
    assert!(!client.request(Request::HostStart).await.ok);
    assert_eq!(
        effects.state.lock().unwrap().trace,
        ["closure-query", "Inspect"]
    );
    client
        .request(Request::AbortUnapplied {
            diagnostic: "missing inspect evidence".into(),
        })
        .await
        .check()
        .unwrap();
    client.request(Request::Finish).await.check().unwrap();
    client.close().await.unwrap();
}

#[tokio::test]
async fn vm_owner_backend_declared_runtime_deadline_is_failure_not_pending() {
    let root = Temp::new();
    let effects = mock(&root);
    effects.state.lock().unwrap().block_apply = true;
    let mut client = Client::new(&root, effects).await;
    client.hello().await;
    client
        .lock("host-a", "run-a", false, false)
        .await
        .check()
        .unwrap();
    let (mut manifest, unit, _) = manifest();
    for guest in manifest.guests.values_mut() {
        guest.readiness.deadline_seconds = 1;
    }
    client
        .request(Request::Prepare {
            unit: Box::new(unit),
            manifest: Box::new(manifest),
            profile: path("profile"),
        })
        .await
        .check()
        .unwrap();
    client.request(Request::HostStart).await.check().unwrap();
    let reply = tokio::time::timeout(
        Duration::from_secs(8),
        client.request(Request::Apply { mode: Mode::Live }),
    )
    .await
    .unwrap();
    assert!(!reply.ok);
    assert!(reply.error.unwrap().contains("deadline expired"));
    assert!(!matches!(
        reply.journal.unwrap().state,
        State::Pending | State::Confirmed
    ));
    client
        .request(Request::Recover {
            auto_rollback: true,
            primary: "live restart deadline".into(),
        })
        .await
        .check()
        .unwrap();
    client.request(Request::Finish).await.check().unwrap();
    client.close().await.unwrap();
}

#[tokio::test]
async fn vm_owner_backend_abrupt_crash_admission_persists_unknown_before_reconcile() {
    let root = Temp::new();
    let effects = mock(&root);
    let mut client = Client::new(&root, effects.clone()).await;
    client.hello().await;
    client
        .lock("host-a", "run-a", false, false)
        .await
        .check()
        .unwrap();
    client.prepare().await.check().unwrap();
    client.request(Request::HostStart).await.check().unwrap();
    client.server.abort();
    drop(client.output);
    drop(client.input);
    assert!(client.server.await.is_err());
    assert_eq!(root.journal("host-a").state, State::Mutating);
    let mut next = Client::new(&root, effects).await;
    next.hello().await;
    assert!(!next.lock("host-a", "run-b", false, false).await.ok);
    assert_eq!(root.journal("host-a").state, State::Unknown);
    next.lock("host-a", "run-b", false, true)
        .await
        .check()
        .unwrap();
    next.request(Request::Reconcile { restore: true })
        .await
        .check()
        .unwrap();
    next.request(Request::Finish).await.check().unwrap();
    next.close().await.unwrap();
}

#[tokio::test]
async fn vm_owner_backend_persistence_failure_poison_prevents_subsequent_effects() {
    let root = Temp::new();
    let effects = mock(&root);
    let mut client = Client::new(&root, effects.clone()).await;
    client.hello().await;
    client
        .lock("host-a", "run-a", false, false)
        .await
        .check()
        .unwrap();
    client.prepare().await.check().unwrap();
    std::fs::create_dir(root.0.join("host-a/journal.tmp")).unwrap();
    assert!(!client.request(Request::HostStart).await.ok);
    assert!(!client.request(Request::Apply { mode: Mode::Live }).await.ok);
    assert!(
        !client
            .request(Request::Recover {
                auto_rollback: true,
                primary: "persistence failed".into()
            })
            .await
            .ok
    );
    assert!(
        !effects
            .state
            .lock()
            .unwrap()
            .trace
            .iter()
            .any(|s| s == "Apply" || s == "Restore")
    );
    assert!(client.close().await.is_err());
    assert_eq!(root.journal("host-a").state, State::Prepared);
}

#[tokio::test]
async fn vm_owner_backend_disabled_rollback_is_explicit_and_never_restores() {
    let root = Temp::new();
    let effects = mock(&root);
    effects.state.lock().unwrap().mismatch = true;
    let mut client = Client::new(&root, effects.clone()).await;
    client.hello().await;
    client
        .lock("host-a", "run-a", false, false)
        .await
        .check()
        .unwrap();
    client.prepare().await.check().unwrap();
    client.request(Request::HostStart).await.check().unwrap();
    assert!(!client.request(Request::Apply { mode: Mode::Live }).await.ok);
    let journal = client
        .request(Request::Recover {
            auto_rollback: false,
            primary: "readiness failed".into(),
        })
        .await
        .check()
        .unwrap()
        .unwrap();
    assert_eq!(journal.state, State::FailedUnrecovered);
    assert!(journal.primary_failure.is_some());
    assert!(journal.recovery_failure.is_none());
    assert!(
        !effects
            .state
            .lock()
            .unwrap()
            .trace
            .iter()
            .any(|s| s == "Restore")
    );
    client.request(Request::Finish).await.check().unwrap();
    client.close().await.unwrap();
    assert_eq!(root.journal("host-a").state, State::FailedUnrecovered);
    let mut next = Client::new(&root, effects).await;
    next.hello().await;
    assert!(!next.lock("host-a", "run-b", false, false).await.ok);
    assert!(next.close().await.is_err());
}

#[tokio::test]
async fn vm_owner_backend_session_loss_cancels_owned_effect_without_detached_recovery() {
    let root = Temp::new();
    let effects = mock(&root);
    effects.state.lock().unwrap().block_apply = true;
    let mut client = Client::new(&root, effects.clone()).await;
    client.hello().await;
    client
        .lock("host-a", "run-a", false, false)
        .await
        .check()
        .unwrap();
    client.prepare().await.check().unwrap();
    client.request(Request::HostStart).await.check().unwrap();
    let bytes = serde_json::to_vec(&Request::Apply { mode: Mode::Live }).unwrap();
    client.output.write_all(&bytes).await.unwrap();
    client.output.write_all(b"\n").await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), effects.barrier.notified())
        .await
        .unwrap();
    assert!(client.close().await.is_err());
    assert_eq!(root.journal("host-a").state, State::Unknown);
    let trace = &effects.state.lock().unwrap().trace;
    assert!(!trace.iter().any(|s| s == "Verify" || s == "Restore"));
}
