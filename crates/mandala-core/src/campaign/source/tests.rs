use super::*;
use serde_json::json;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

thread_local! { static HARNESS: RefCell<Option<Harness>> = const { RefCell::new(None) }; }
struct Harness {
    root: PathBuf,
    calls: Rc<RefCell<Fake>>,
}
#[derive(Default)]
struct Fake {
    calls: Vec<(String, Vec<String>)>,
    archives: usize,
    move_source: bool,
    move_input: bool,
    unknown_output: bool,
    missing_system: bool,
    wrapper: bool,
    wrapper_wrong: bool,
    replacement: bool,
    unavailable: bool,
    move_on_profile: bool,
    dirty: bool,
    stripped_metadata: bool,
    stripped_profile: bool,
    relative: bool,
    unsupported_version: bool,
    metadata_epoch: u64,
}
pub(super) fn state_root() -> Option<PathBuf> {
    HARNESS.with(|h| h.borrow().as_ref().map(|h| h.root.clone()))
}
pub(super) fn run(program: &str, args: &[String]) -> Option<io::Result<Vec<u8>>> {
    HARNESS.with(|h| {
        h.borrow()
            .as_ref()
            .map(|h| fake_output(&mut h.calls.borrow_mut(), program, args))
    })
}
struct Fixture {
    root: PathBuf,
    fake: Rc<RefCell<Fake>>,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "mandala-source-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let source = root.join("input");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("flake.nix"),
            include_str!("../../../tests/fixtures/campaign-metadata/flake.nix"),
        )
        .unwrap();
        std::fs::create_dir_all(source.join("child")).unwrap();
        std::fs::write(
            source.join("child/flake.nix"),
            include_str!("../../../tests/fixtures/campaign-metadata/child/flake.nix"),
        )
        .unwrap();
        std::fs::write(source.join("flake.lock"), "{}").unwrap();
        let fake = Rc::new(RefCell::new(Fake::default()));
        HARNESS.with(|h| {
            *h.borrow_mut() = Some(Harness {
                root: root.clone(),
                calls: Rc::clone(&fake),
            })
        });
        Self { root, fake }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        HARNESS.with(|h| *h.borrow_mut() = None);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn locks() -> Value {
    json!({"version":7,"root":"root","nodes":{"root":{"inputs":{"local":"local"}},"local":{"original":{"type":"path","path":"/fixtures/mutable-input"},"locked":{"type":"path","path":"/fixtures/mutable-input","narHash":"sha256-original"}}}})
}
fn inventory() -> Value {
    let settings = |name: &str| json!({"activation":"switch","hostname":name,"sshUser":"root","sshPort":22,"identityFile":null,"autoRollback":true,"fastConnection":true});
    json!({"schemaVersion":1,"members":{"node-a":{"name":"node-a","deployment":{"deployRs":{"enable":true}}},"node-b":{"name":"node-b","deployment":{"deployRs":{"enable":true}}},"node-c":{"name":"node-c","deployment":{"deployRs":{"enable":true}}},"router":{"name":"router","deployment":{"deployRs":{"enable":false}}}},"groups":{"fleet":["node-a","node-b","router"]},"projections":{"deploy":{"nodes":["node-a","node-b","node-c"],"settings":{"node-a":settings("node-a"),"node-b":settings("node-b"),"node-c":settings("node-c")}}}})
}
fn metadata(fake: &Fake) -> Value {
    let path = if fake.replacement {
        "/nix/store/replacement-source"
    } else if fake.move_source && fake.archives > 0 {
        "/nix/store/moved-source"
    } else {
        "/nix/store/original-source"
    };
    let mut locked = json!({"type":"git","url":"file:///fixtures/acme","narHash":format!("sha256-{}",path.rsplit('/').next().unwrap()),"lastModified":123+fake.metadata_epoch,"revCount":1});
    if fake.dirty {
        locked["dirtyRev"] = json!("revision-a-dirty");
        locked["dirtyShortRev"] = json!("revisio-dirty");
    } else {
        locked["rev"] = json!("revision-a");
    }
    let mut graph = locks();
    if fake.relative {
        graph["nodes"]["local"] = json!({"locked":{"type":"path","path":"./child"},"original":{"type":"path","path":"./child"},"parent":[]});
    }
    json!({"path":path,"revision":if fake.dirty{Value::Null}else{json!("revision-a")},"locked":locked,"locks":graph})
}
fn json_bytes(value: Value) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&value).map_err(invalid_json)
}
fn fake_output(fake: &mut Fake, program: &str, args: &[String]) -> io::Result<Vec<u8>> {
    fake.calls.push((program.into(), args.to_vec()));
    if program == "nix-instantiate" {
        let path = args[4]
            .split("drvPath = \"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        assert!(path.ends_with(".drv"));
        #[cfg(unix)]
        std::os::unix::fs::symlink(path, &args[1])?;
        return Ok(args[1].as_bytes().to_vec());
    }
    if program == "nix-store" && args[0] == "--add-root" {
        assert!(
            !args[4].ends_with(".drv"),
            "rooting must never realise a derivation"
        );
        #[cfg(unix)]
        std::os::unix::fs::symlink(&args[4], &args[1])?;
        return Ok(args[4].as_bytes().to_vec());
    }
    if program == "nix-store" && args[0] == "--query" {
        return Ok(if fake.wrapper_wrong {
            b"/nix/store/unrelated-system.drv\n".to_vec()
        } else {
            b"/nix/store/system-a.drv\n".to_vec()
        });
    }
    if program == "git" {
        return Ok(match args[2].as_str() {
            "ls-files" if !args[1].ends_with("module") => {
                b"160000 recorded-revision 0\tmodule\0".to_vec()
            }
            "ls-files" => Vec::new(),
            "rev-parse" => b"actual-revision\n".to_vec(),
            "status" => b" M tracked-file\n".to_vec(),
            _ => panic!("unexpected Git call {args:?}"),
        });
    }
    assert_eq!(program, "nix", "unexpected external program");
    match (args[0].as_str(), args.get(1).map(String::as_str)) {
        ("--version", _) => Ok(if fake.unsupported_version {
            b"nix (Nix) 2.31.0".to_vec()
        } else {
            b"nix (Nix) 2.34.9".to_vec()
        }),
        ("flake", Some("metadata")) => json_bytes(metadata(fake)),
        ("flake", Some("archive")) => {
            let moved = fake.archives > 0;
            fake.archives += 1;
            json_bytes(
                json!({"path":if fake.replacement {"/nix/store/replacement-source"} else if fake.move_source && moved {"/nix/store/moved-source"} else {"/nix/store/original-source"},"inputs":{"local":if fake.relative {json!({"inputs":{}})}else{json!({"path":if fake.move_input && moved {"/nix/store/moved-input"} else {"/nix/store/input-a"},"inputs":{}})}}}),
            )
        }
        ("path-info", _) if fake.unavailable => Err(invalid("fixture store identity unavailable")),
        ("path-info", _) => json_bytes(
            json!({args[2].clone():{"narHash":format!("sha256-{}",args[2].rsplit('/').next().unwrap())}}),
        ),
        ("store", Some("add-path")) => {
            let path = Path::new(args.last().unwrap());
            let graph: Value = serde_json::from_slice(&std::fs::read(path.join("graph.json"))?)
                .map_err(invalid_json)?;
            assert_eq!(
                graph["nodes"]["root"]["source"]["metadata"]["lastModified"],
                123 + fake.metadata_epoch
            );
            if fake.relative {
                assert_eq!(graph["nodes"]["local"]["relative"], "./child");
            } else {
                assert_eq!(
                    graph["nodes"]["local"]["source"]["path"],
                    "/nix/store/input-a"
                );
            }
            assert!(
                std::fs::read_to_string(path.join("replay.nix"))?.contains("node.source.metadata")
            );
            Ok(if fake.replacement {
                b"/nix/store/replacement-frozen-source\n".to_vec()
            } else {
                b"/nix/store/frozen-source\n".to_vec()
            })
        }
        ("hash", Some("path")) => Ok(b"sha256-module-source\n".to_vec()),
        ("eval", _) if args.contains(&"--impure".into()) => {
            if args
                .last()
                .unwrap()
                .starts_with("let locked = builtins.fromJSON")
            {
                let quoted = args
                    .last()
                    .unwrap()
                    .split("builtins.readFile ")
                    .nth(1)
                    .unwrap()
                    .split(");")
                    .next()
                    .unwrap();
                let path: String = serde_json::from_str(quoted).unwrap();
                let parameters: Value = serde_json::from_slice(&std::fs::read(path)?).unwrap();
                let root = parameters["type"] == "git";
                let path = if root {
                    if fake.replacement {
                        "/nix/store/replacement-source"
                    } else {
                        "/nix/store/original-source"
                    }
                } else {
                    "/nix/store/input-a"
                };
                let mut meta = json!({"narHash":format!("sha256-{}",path.rsplit('/').next().unwrap()),"lastModified":parameters["lastModified"].as_u64().unwrap_or(123),"lastModifiedDate":"19700101000203"});
                if root {
                    meta["submodules"] = json!(false);
                }
                for key in ["rev", "revCount", "dirtyRev", "dirtyShortRev"] {
                    if !parameters[key].is_null() {
                        meta[key] = parameters[key].clone();
                    }
                }
                json_bytes(json!({"path":path,"metadata":meta}))
            } else {
                assert!(args.last().unwrap().starts_with("builtins.fetchTree"));
                json_bytes(json!({"outPath":"/nix/store/module-source"}))
            }
        }
        ("eval", _) => {
            assert!(!args.contains(&"--impure".into()));
            assert!(
                args.windows(3)
                    .any(|window| window == ["--option", "pure-eval", "true"]),
                "fleet evaluation must explicitly pin pure mode"
            );
            if args.contains(&"--expr".into()) {
                return json_bytes(json!([]));
            }
            let reference = args.iter().find(|arg| arg.contains('#')).unwrap();
            assert!(
                reference.starts_with("path:/nix/store/frozen-source#")
                    || reference.starts_with("fixture:acme#"),
                "must not evaluate a live source: {reference}"
            );
            if reference.ends_with("#nixosConfigurations") {
                let suffix = if fake.stripped_metadata && reference.starts_with("path:") {
                    "-metadata-lost"
                } else {
                    ""
                };
                json_bytes(Value::Object(["node-a","node-b","node-c"].into_iter().map(|name|{let part=if name=="node-a"{"a"}else{"b"};(name.into(),json!({"path":if fake.missing_system{Value::Null}else{json!(format!("/nix/store/system-{part}"))},"drv":format!("/nix/store/system-{part}{suffix}.drv")}))}).collect()))
            } else if reference.ends_with("#mandala") {
                json_bytes(inventory())
            } else if reference.ends_with(".drvPath") {
                Ok(b"/nix/store/system-a.drv".to_vec())
            } else if reference.ends_with(".outPath") {
                if fake.missing_system {
                    json_bytes(Value::Null)
                } else {
                    json_bytes(Value::String(
                        if reference.contains(".node-a.") {
                            "/nix/store/system-a"
                        } else {
                            "/nix/store/system-b"
                        }
                        .into(),
                    ))
                }
            } else {
                panic!("unexpected evaluation {args:?}");
            }
        }
        ("build", _) => {
            if fake.move_on_profile {
                fake.replacement = true;
            }
            assert!(
                args.windows(3)
                    .any(|window| window == ["--option", "pure-eval", "true"])
            );
            assert!(args.contains(&"--dry-run".into()), "capture cannot build");
            json_bytes(Value::Array(args.iter().filter(|arg| arg.contains("#deploy.nodes.")).map(|arg| {
                let name = if arg.contains(".node-a.") {"a"} else {"b"};
                json!({"drvPath":format!("/nix/store/profile-{name}{}.drv",if fake.stripped_profile&&arg.starts_with("path:"){"-metadata-lost"}else{""}),"outputs":{"out":if fake.unknown_output {Value::Null} else {Value::String(if fake.wrapper { format!("/nix/store/wrapper-{name}") } else { format!("/nix/store/system-{name}") })}}})
            }).collect()))
        }
        _ => panic!("unexpected Nix call {args:?}"),
    }
}

#[test]
fn full_capture_freezes_local_lock_nodes_and_never_evaluates_live_inventory() {
    let fixture = Fixture::new();
    let capture =
        Capture::new("fixture:acme", "@fleet,!node-b", "operator-alice", 2, true).unwrap();
    assert_eq!(
        capture.target.selection.members,
        ["node-a", "node-b", "router"]
    );
    assert_eq!(
        capture.target.selection.deploy_members,
        ["node-a", "node-b"]
    );
    assert_eq!(capture.plan.targets, ["node-a"]);
    assert_eq!(
        capture.target.initial_exclusions["node-b"].reason,
        "!node-b"
    );
    assert_eq!(
        capture.target.initial_exclusions["router"].kind,
        ExclusionKind::NonDeployCapable
    );
    assert_eq!(
        capture.target.hosts["node-b"].activation,
        ActivationMode::Boot
    );
    assert_eq!(
        capture.target.hosts["node-b"].profile,
        "/nix/store/system-b"
    );
    let store = Store::new(fixture.root.join("campaigns"));
    let c = capture.bind(&store).unwrap();
    assert!(c.attempts.is_empty());
    assert!(!fixture.root.join("runs").exists());
    let roots = store.path(&c.campaign_id).unwrap().join("roots");
    assert!(std::fs::symlink_metadata(roots.join("frozen")).is_ok());
    drop(capture);
    assert!(std::fs::symlink_metadata(roots.join("derivation-0")).is_ok());
    assert!(
        fixture
            .fake
            .borrow()
            .calls
            .iter()
            .all(|(program, _)| program != "ssh")
    );
}

#[test]
fn durable_source_references_reject_inline_credentials_and_opaque_queries() {
    for reference in [
        "git+https://git.acme.example/fleet?rev=abc&shallow=1",
        "git+file:///fixtures/acme?submodules=1",
        "github:acme/fleet?ref=main",
    ] {
        assert!(safe_reference(reference));
    }
    for reference in [
        "https://user:secret@git.acme.example/fleet",
        "https://opaque-token@git.acme.example/fleet",
        "github:acme/fleet?accessToken=secret",
        "https://acme.example/source?X-Amz-Signature=secret",
        "https://acme.example/source?%61ccess_token=secret",
    ] {
        assert!(!safe_reference(reference));
    }
}

#[test]
fn dirty_revision_labels_preserve_the_available_revision_without_claiming_content_equality() {
    assert_eq!(
        source_revision(&json!({"dirtyRevision":"revision-a-dirty"})).as_deref(),
        Some("revision-a")
    );
    assert_eq!(
        source_revision(&json!({"dirtyRev":"revision-b-dirty"})).as_deref(),
        Some("revision-b")
    );
    assert_eq!(
        source_revision(&json!({"revision":"revision-c"})).as_deref(),
        Some("revision-c")
    );
    assert!(source_revision(&json!({})).is_none());
}

#[test]
fn source_observation_does_not_evaluate_inventory_or_resolve_profiles() {
    let fixture = Fixture::new();
    let source = observe("fixture:acme").unwrap();
    assert_eq!(source.snapshot.store_path, "/nix/store/frozen-source");
    assert_eq!(source.inputs["local"].store_path, "/nix/store/input-a");
    assert!(
        fixture
            .fake
            .borrow()
            .calls
            .iter()
            .all(|(_, args)| !args.iter().any(|arg| arg.contains('#')) && args[0] != "build")
    );
    assert!(!fixture.root.join("campaigns").exists());
}

#[test]
fn capture_refuses_root_or_nested_source_movement_without_binding() {
    for nested in [false, true] {
        let fixture = Fixture::new();
        if nested {
            fixture.fake.borrow_mut().move_input = true;
        } else {
            fixture.fake.borrow_mut().move_source = true;
        }
        assert!(Capture::new("fixture:acme", "@fleet", "operator-alice", 2, false).is_err());
        assert!(!fixture.root.join("campaigns").exists());
        assert!(!fixture.root.join("runs").exists());
    }
}

#[test]
fn metadata_faithful_capsule_passes_and_stripped_profile_or_system_metadata_refuses() {
    for profile in [false, true] {
        let fixture = Fixture::new();
        fixture.fake.borrow_mut().dirty = true;
        let capture = Capture::new("fixture:acme", "node-a", "operator-alice", 2, false).unwrap();
        assert!(capture.target.source.dirty);
        assert!(capture.target.source.root.revision.is_some());
        drop(capture);
        if profile {
            fixture.fake.borrow_mut().stripped_profile = true;
        } else {
            fixture.fake.borrow_mut().stripped_metadata = true;
        }
        let error = Capture::new("fixture:acme", "node-a", "operator-alice", 2, false)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("node-a"));
        assert!(error.contains("fetcher metadata the frozen graph can't reproduce"));
        assert!(!fixture.root.join("campaigns").exists());
    }
}

#[test]
fn source_identity_includes_fetcher_metadata_not_capsule_implementation_bytes() {
    let fixture = Fixture::new();
    let before = observe("fixture:acme").unwrap();
    let mut artifact_only = before.clone();
    artifact_only.snapshot.store_path = "/nix/store/other-capsule".into();
    artifact_only.snapshot.content_digest = "sha256-other-code".into();
    assert!(before.same_content(&artifact_only));
    assert_eq!(
        crate::campaign::views::source_summary(&before).unwrap()["content_identity"],
        crate::campaign::views::source_summary(&artifact_only).unwrap()["content_identity"]
    );
    fixture.fake.borrow_mut().metadata_epoch = 1;
    let after = observe("fixture:acme").unwrap();
    assert_eq!(before.root, after.root);
    assert_ne!(before.graph_digest, after.graph_digest);
    assert!(!before.same_content(&after));
}

#[test]
fn relative_inputs_inherit_archived_parent_metadata_and_unsupported_nix_refuses() {
    let fixture = Fixture::new();
    fixture.fake.borrow_mut().relative = true;
    let source = observe("fixture:acme").unwrap();
    assert_eq!(source.inputs["local"], source.root);
    fixture.fake.borrow_mut().calls.clear();
    fixture.fake.borrow_mut().unsupported_version = true;
    assert!(
        observe("fixture:acme")
            .err()
            .unwrap()
            .to_string()
            .contains("supported Nix 2.34")
    );
    assert_eq!(fixture.fake.borrow().calls.len(), 1);
}

#[test]
fn concurrent_capture_directories_never_share_root_ownership() {
    let fixture = Fixture::new();
    let base = fixture.root.join("captures");
    let threads: Vec<_> = (0..32)
        .map(|_| {
            let base = base.clone();
            std::thread::spawn(move || claim_capture_dir(&base).unwrap())
        })
        .collect();
    let guards: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    let paths: BTreeSet<_> = guards.iter().map(|guard| guard.0.clone()).collect();
    assert_eq!(paths.len(), 32);
    drop(guards);
    assert!(paths.iter().all(|path| !path.exists()));
}

#[test]
fn source_movement_after_target_evaluation_is_a_binding_barrier() {
    let fixture = Fixture::new();
    fixture.fake.borrow_mut().move_on_profile = true;
    assert!(Capture::new("fixture:acme", "node-a", "operator-alice", 2, false).is_err());
    assert!(!fixture.root.join("campaigns").exists());
    assert!(!fixture.root.join("runs").exists());
    assert!(
        fixture
            .fake
            .borrow()
            .calls
            .iter()
            .any(|(_, args)| args[0] == "build")
    );
}

#[test]
fn unknown_profile_or_system_path_prevents_binding_and_host_effects() {
    for system in [false, true] {
        let fixture = Fixture::new();
        if system {
            fixture.fake.borrow_mut().missing_system = true;
        } else {
            fixture.fake.borrow_mut().unknown_output = true;
        }
        assert!(Capture::new("fixture:acme", "node-a", "operator-alice", 2, false).is_err());
        assert!(!fixture.root.join("campaigns").exists());
        assert!(!fixture.root.join("runs").exists());
    }
}

#[test]
fn wrapper_and_system_expectations_are_distinct_and_correspondence_is_required() {
    for wrong in [false, true] {
        let fixture = Fixture::new();
        fixture.fake.borrow_mut().wrapper = true;
        fixture.fake.borrow_mut().wrapper_wrong = wrong;
        let result = Capture::new("fixture:acme", "node-a", "operator-alice", 2, false);
        if wrong {
            assert!(result.is_err());
        } else {
            let capture = result.unwrap();
            assert_eq!(
                capture.target.hosts["node-a"].profile,
                "/nix/store/wrapper-a"
            );
            assert_eq!(
                capture.target.hosts["node-a"].system_toplevel,
                "/nix/store/system-a"
            );
        }
        assert!(!fixture.root.join("campaigns").exists());
    }
}

#[test]
fn all_excluded_and_non_deploy_only_capture_do_not_build_or_launch() {
    let fixture = Fixture::new();
    let capture = Capture::new(
        "fixture:acme",
        "@fleet,!node-a,!node-b",
        "operator-alice",
        2,
        false,
    )
    .unwrap();
    assert!(capture.plan.targets.is_empty());
    assert_eq!(capture.target.hosts.len(), 2);
    drop(capture);
    fixture.fake.borrow_mut().calls.clear();
    let capture = Capture::new("fixture:acme", "router", "operator-alice", 2, false).unwrap();
    assert!(capture.target.hosts.is_empty());
    assert!(
        fixture
            .fake
            .borrow()
            .calls
            .iter()
            .all(|(_, args)| args[0] != "build")
    );
}

#[test]
fn chained_follows_and_shared_nodes_have_full_content_identities() {
    let locks = json!({"root":"root","nodes":{"root":{"inputs":{"base":"base","alias":["base"],"chain":["alias"],"tool":"tool"}},"base":{"locked":{}},"tool":{"inputs":{"nested":["chain"]},"locked":{}}}});
    let archive = json!({"path":"/nix/store/root","inputs":{"base":{"path":"/nix/store/base","inputs":{}},"tool":{"path":"/nix/store/tool","inputs":{}}}});
    let found = archived_nodes(&locks, &archive).unwrap();
    assert_eq!(found.len(), 3);
    let nodes: BTreeMap<_, _> = found
        .into_iter()
        .map(|(name, path)| {
            (
                name,
                SourceNode {
                    content_digest: format!("sha256-{path}"),
                    store_path: path,
                    revision: None,
                },
            )
        })
        .collect();
    let identities = input_identities(&locks, &nodes).unwrap();
    assert_eq!(identities["alias"], identities["chain"]);
    assert_eq!(identities["base"], identities["tool/nested"]);
}

#[test]
fn submodule_capture_records_gitlink_actual_revision_dirty_state_and_content() {
    let fixture = Fixture::new();
    let checkout = fixture.root.join("git-source");
    let archived = fixture.root.join("archive");
    std::fs::create_dir_all(checkout.join("module")).unwrap();
    std::fs::create_dir_all(archived.join("module")).unwrap();
    std::fs::write(checkout.join(".git"), "gitdir: fixture").unwrap();
    std::fs::write(checkout.join("module/.git"), "gitdir: fixture-module").unwrap();
    let mut modules = BTreeMap::new();
    capture_submodules(
        &checkout,
        &archived,
        "",
        &fixture.root.join("roots"),
        &mut modules,
    )
    .unwrap();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules["module"].gitlink_revision, "recorded-revision");
    assert_eq!(modules["module"].checkout_revision, "actual-revision");
    assert!(modules["module"].dirty);
    assert_eq!(
        modules["module"].content.content_digest,
        "sha256-module-source"
    );
    std::fs::remove_dir_all(archived.join("module")).unwrap();
    assert!(
        capture_submodules(
            &checkout,
            &archived,
            "",
            &fixture.root.join("other-roots"),
            &mut BTreeMap::new()
        )
        .is_err()
    );
}

#[test]
fn creation_confirmation_and_explicit_preview_have_no_implicit_runs() {
    use crate::campaign::api::{self, CreateRequest};
    let f = Fixture::new();
    let store = Store::new(f.root.join("campaigns"));
    let refused = api::create(
        &store,
        "fixture:acme",
        CreateRequest {
            selector: "node-a",
            actor: "operator-alice",
            boot: false,
            preview: false,
            confirm: None,
            throttle: 2,
        },
    )
    .unwrap();
    assert_eq!(refused["confirmation_required"], true);
    assert_eq!(refused["required_confirm"], "node-a");
    assert!(!store.root.exists());
    assert!(!f.root.join("runs").exists());
    let preview = api::create(
        &store,
        "fixture:acme",
        CreateRequest {
            selector: "node-a",
            actor: "operator-alice",
            boot: false,
            preview: true,
            confirm: None,
            throttle: 2,
        },
    )
    .unwrap();
    let c = store
        .load(preview["campaign_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(c.created_by, "operator-alice");
    assert!(c.attempts.is_empty());
    assert!(c.outcomes.is_empty());
    assert_eq!(preview["ready_to_deploy"], false);
    assert!(!f.root.join("runs").exists());
}

#[derive(Default)]
struct ExecutionState {
    build_failures: BTreeSet<String>,
    activation_failures: BTreeSet<String>,
    trace: Vec<(String, String)>,
}
struct TestExecutor {
    store: Store,
    id: String,
    state: std::sync::Mutex<ExecutionState>,
}
impl crate::campaign::execution::Executor for TestExecutor {
    fn build(
        &self,
        run: &crate::engines::deploy::RegisteredDeployRun,
        resolved: &[(String, String)],
    ) -> Result<crate::engines::deploy::BuiltProfiles, crate::engines::deploy::BuildError> {
        let c = self.store.load(&self.id).unwrap();
        let attempt = c.attempts.last().unwrap();
        assert_eq!(attempt.run_id.as_deref(), Some(run.run_id.as_str()));
        assert_eq!(attempt.state, AttemptState::Running);
        let meta = registry::read_meta(&run.path);
        assert_eq!(meta["campaign_target_digest"], c.target_digest);
        assert_eq!(meta["campaign_id"], c.campaign_id);
        let mut built = crate::engines::deploy::BuiltProfiles {
            paths: BTreeMap::new(),
            failures: BTreeMap::new(),
        };
        let mut state = self.state.lock().unwrap();
        for (name, (drv, output)) in run.plan.targets.iter().zip(resolved) {
            let host = &c.target.hosts[name];
            assert_eq!(drv, &format!("{}^{}", host.derivation, host.output));
            assert_eq!(output, &host.profile);
            assert_eq!(run.plan.settings[name]["hostname"], name.as_str());
            state.trace.push(("build".into(), name.clone()));
            if state.build_failures.contains(name) {
                built
                    .failures
                    .insert(name.clone(), "fixture build failure".into());
            } else {
                built.paths.insert(name.clone(), PathBuf::from(output));
            }
        }
        Ok(built)
    }
    fn publish(
        &self,
        run: &crate::engines::deploy::RegisteredDeployRun,
        built: &crate::engines::deploy::BuiltProfiles,
    ) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        for name in built.paths.keys() {
            state.trace.push(("publish".into(), name.clone()));
        }
        registry::update_meta(&run.path,json!({"publish":{"outcome":"published","path_count":built.paths.values().collect::<BTreeSet<_>>().len()}}).as_object().unwrap().clone())
    }
    fn fan_out<'a>(
        &'a self,
        run: &'a crate::engines::deploy::RegisteredDeployRun,
        built: &'a crate::engines::deploy::BuiltProfiles,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = io::Result<crate::engines::deploy::DeployOutcome>>
                + Send
                + 'a,
        >,
    > {
        use crate::runner::{EventWriter, HostState};
        let mut results = BTreeMap::new();
        let mut state = self.state.lock().unwrap();
        for name in &run.plan.targets {
            if built.paths.contains_key(name) {
                assert!(
                    state
                        .trace
                        .iter()
                        .any(|(op, host)| op == "publish" && host == name),
                    "publish must precede every eligible host effect"
                );
                assert!(registry::read_meta(&run.path).contains_key("publish"));
                state.trace.push(("copy".into(), name.clone()));
                if !run.plan.dry_activate {
                    state.trace.push(("activate".into(), name.clone()));
                }
            }
            let failed =
                built.failures.contains_key(name) || state.activation_failures.contains(name);
            let outcome = if failed {
                HostState::Failed
            } else {
                HostState::Confirmed
            };
            let writer = EventWriter::new(&run.path, name, name, "deploy").unwrap();
            if failed {
                writer
                    .emit(
                        "status",
                        json!({"state":"done","rc":1}).as_object().unwrap().clone(),
                    )
                    .unwrap();
            } else {
                writer
                    .emit(
                        "milestone",
                        json!({"milestone":"confirm"}).as_object().unwrap().clone(),
                    )
                    .unwrap();
            }
            results.insert(
                name.clone(),
                crate::engines::deploy::HostDeployResult {
                    host: name.clone(),
                    state: outcome,
                    error: failed.then(|| "fixture failure".into()),
                },
            );
        }
        let failed = results
            .values()
            .filter(|r| r.state == HostState::Failed)
            .count();
        let summary = crate::engines::deploy::DeploySummary {
            total: results.len(),
            confirmed: results.len() - failed,
            failed,
            reboot_pending: 0,
            rolled_back: 0,
        };
        let rc = if failed > 0 { 1 } else { 0 };
        registry::update_meta(
            &run.path,
            json!({"rc":rc,"pid":null}).as_object().unwrap().clone(),
        )
        .unwrap();
        let outcome = crate::engines::deploy::DeployOutcome {
            results,
            summary,
            vm_summary: None,
            vm_rows: Vec::new(),
            process_rc: 0,
            rc,
        };
        Box::pin(async move { Ok(outcome) })
    }
}

#[tokio::test]
async fn create_and_deploy_retry_preserves_success_and_refuses_moved_or_unavailable_source() {
    use crate::campaign::{api, execution};
    let f = Fixture::new();
    let store = Store::new(f.root.join("campaigns"));
    let _runs = registry::test_hooks::install_runs_base(f.root.join("runs"));
    let created = api::create(
        &store,
        "fixture:acme",
        api::CreateRequest {
            selector: "node-a,node-b,node-c",
            actor: "operator-alice",
            boot: false,
            preview: false,
            confirm: Some("node-a,node-b,node-c"),
            throttle: 2,
        },
    )
    .unwrap();
    assert_eq!(created["ready_to_deploy"], true);
    let id = created["campaign_id"].as_str().unwrap().to_string();
    let executor = TestExecutor {
        store: store.clone(),
        id: id.clone(),
        state: std::sync::Mutex::new(ExecutionState {
            build_failures: BTreeSet::from(["node-c".into()]),
            activation_failures: BTreeSet::from(["node-b".into()]),
            trace: Vec::new(),
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
    f.fake.borrow_mut().calls.clear();
    let first = execution::deploy_with_executor(
        &store,
        &id,
        request(0, Some("node-a,node-b,node-c")),
        |run| {
            assert!(f.root.join("runs").join(run).join("meta.json").exists());
            Ok(())
        },
        &executor,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(first.outcome.rc, 1);
    assert_eq!(
        store
            .load(&id)
            .unwrap()
            .eligible(AttemptKind::Deploy, None)
            .unwrap(),
        ["node-b", "node-c"]
    );
    let trace = executor.state.lock().unwrap().trace.clone();
    f.fake.borrow_mut().replacement = true;
    let error = execution::deploy_with_executor(
        &store,
        &id,
        request(1, Some("node-b,node-c")),
        |_| Ok(()),
        &executor,
    )
    .await
    .err()
    .unwrap();
    assert!(error.to_string().contains("source changed"));
    assert_eq!(store.load(&id).unwrap().attempts.len(), 1);
    assert_eq!(executor.state.lock().unwrap().trace, trace);
    f.fake.borrow_mut().replacement = false;
    f.fake.borrow_mut().unavailable = true;
    f.fake.borrow_mut().calls.clear();
    let error = execution::deploy_with_executor(
        &store,
        &id,
        request(1, Some("node-b,node-c")),
        |_| Ok(()),
        &executor,
    )
    .await
    .err()
    .unwrap();
    assert!(error.to_string().contains("target-unavailable"));
    assert!(
        f.fake
            .borrow()
            .calls
            .iter()
            .all(|(_, args)| args[0] == "path-info")
    );
    f.fake.borrow_mut().unavailable = false;
    f.fake.borrow_mut().calls.clear();
    {
        let mut state = executor.state.lock().unwrap();
        state.build_failures.clear();
        state.activation_failures.clear();
    }
    let second = execution::deploy_with_executor(
        &store,
        &id,
        request(1, Some("node-b,node-c")),
        |_| Ok(()),
        &executor,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(second.outcome.rc, 0);
    let c = store.load(&id).unwrap();
    assert_eq!(c.attempts.len(), 2);
    assert_eq!(c.outcomes["node-a"].run_id, first.run_id);
    assert_eq!(c.outcomes["node-b"].run_id, second.run_id);
    assert_eq!(
        registry::read_meta(&f.root.join("runs").join(&first.run_id))["rc"],
        1
    );
    let trace = executor.state.lock().unwrap().trace.clone();
    assert_eq!(
        trace
            .iter()
            .filter(|(op, host)| op == "activate" && host == "node-a")
            .count(),
        1
    );
    assert_eq!(
        trace
            .iter()
            .filter(|(op, host)| op == "copy" && host == "node-a")
            .count(),
        1
    );
    assert!(
        f.fake.borrow().calls.iter().all(|(_, args)| !args
            .iter()
            .any(|arg| arg.contains("#mandala")
                || arg.contains("#nixosConfigurations")
                || arg.contains("#deploy.nodes")))
    );
    f.fake.borrow_mut().unavailable = true;
    f.fake.borrow_mut().calls.clear();
    assert!(
        execution::deploy_with_executor(
            &store,
            &id,
            request(2, None),
            |_| panic!("empty retry must not register"),
            &executor
        )
        .await
        .unwrap()
        .is_none()
    );
    assert!(f.fake.borrow().calls.is_empty());
}

struct TestReboot {
    store: Store,
    id: String,
    calls: std::sync::atomic::AtomicUsize,
}
impl crate::campaign::execution::RebootExecutor for TestReboot {
    fn survey(
        &self,
        store: &Store,
        c: &Campaign,
        names: &[String],
    ) -> io::Result<crate::campaign::api::Survey> {
        for name in names {
            std::fs::write(store.state_dir().join(format!("{name}.json")),serde_json::to_vec(&json!({"current":"/nix/store/previous","booted":"/nix/store/previous","system_profile":c.target.hosts[name].system_toplevel,"captured_at":Utc::now().to_rfc3339()})).unwrap())?;
        }
        Ok(crate::campaign::api::Survey {
            rc: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
    fn run(
        &self,
        argv: &[String],
        cwd: &Path,
        _log: &File,
        on_pid: &mut dyn FnMut(u32) -> io::Result<()>,
    ) -> io::Result<(i32, Option<String>)> {
        let c = self.store.load(&self.id)?;
        let attempt = c.attempts.last().unwrap();
        assert_eq!(attempt.kind, AttemptKind::Reboot);
        assert_eq!(attempt.state, AttemptState::Running);
        assert_eq!(argv[0], "nix");
        assert_eq!(argv[1], "run");
        assert_eq!(
            argv[2],
            format!("{}#ans-reboot", c.target.source.snapshot.store_path)
        );
        assert_eq!(cwd, Path::new(&c.target.source.snapshot.store_path));
        assert!(argv.windows(2).any(|args| args == ["-l", "node-a"]));
        assert!(argv.contains(&"reboot_serial=2".into()));
        assert!(argv.contains(&"drain=false".into()));
        let meta = registry::read_meta(
            &self
                .store
                .state_dir()
                .join("runs")
                .join(attempt.run_id.as_ref().unwrap()),
        );
        assert_eq!(meta["campaign_id"], c.campaign_id);
        assert_eq!(meta["kind"], "reboot");
        on_pid(0)?;
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok((0, None))
    }
}

#[test]
fn campaign_reboot_links_before_effects_preserves_wrapper_options_and_does_not_verify() {
    use crate::campaign::{api, execution};
    let f = Fixture::new();
    let store = Store::new(f.root.join("campaigns"));
    let _runs = registry::test_hooks::install_runs_base(f.root.join("runs"));
    let captured = Capture::new("fixture:acme", "node-a", "operator-alice", 2, false).unwrap();
    let c = captured.bind(&store).unwrap();
    let id = c.campaign_id;
    store
        .transaction(&id, |c| {
            let source = c.target.source.clone();
            c.prepare(Dispatch {
                kind: AttemptKind::Deploy,
                generation: 0,
                source: Some(&source),
                narrow: None,
                dry_activate: false,
                confirm: Some("node-a"),
            })?;
            c.link_run(1, "20260102T030406_000001-123")?;
            c.settle(
                1,
                BTreeMap::from([("node-a".into(), HostOutcome::RebootPending)]),
            )
        })
        .unwrap();
    let executor = TestReboot {
        store: store.clone(),
        id: id.clone(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    };
    let request = |confirm| execution::RebootRequest {
        generation: 1,
        confirm,
        narrow: None,
        serial: "2",
        drain: false,
    };
    let refused = execution::reboot_with_executor(
        &store,
        &id,
        request(None),
        |_| panic!("unconfirmed reboot cannot register"),
        &executor,
    )
    .unwrap();
    assert_eq!(refused["confirmation_required"], true);
    assert_eq!(refused["required_confirm"], "node-a");
    assert_eq!(executor.calls.load(Ordering::Relaxed), 0);
    let result = execution::reboot_with_executor(
        &store,
        &id,
        request(Some("node-a")),
        |run| {
            assert!(f.root.join("runs").join(run).join("meta.json").is_file());
            Ok(())
        },
        &executor,
    )
    .unwrap();
    assert_eq!(result["rc"], 0);
    assert_eq!(result["verified"], false);
    assert_eq!(executor.calls.load(Ordering::Relaxed), 1);
    let c = store.load(&id).unwrap();
    assert_eq!(c.outcomes["node-a"].outcome, HostOutcome::RebootPending);
    assert!(c.verification.is_empty());
    let verification = api::verify(&store, &id, false).unwrap();
    assert_eq!(
        verification["complete"], false,
        "reboot command exit cannot replace fresh proof that the target is running"
    );
}

#[test]
fn incomplete_archives_and_cyclic_follows_are_refused() {
    let locks = locks();
    assert!(archived_nodes(&locks, &json!({"path":"/nix/store/root","inputs":{}})).is_err());
    let locks = json!({"root":"root","nodes":{"root":{"inputs":{"a":["b"],"b":["a"]}}}});
    assert!(
        resolve_input(
            &locks,
            &locks["nodes"]["root"]["inputs"]["a"],
            &mut BTreeSet::new()
        )
        .is_err()
    );
}
