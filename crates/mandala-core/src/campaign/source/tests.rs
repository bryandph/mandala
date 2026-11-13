use super::*;
use serde_json::json;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

thread_local! { static HARNESS: RefCell<Option<Harness>> = const { RefCell::new(None) }; }
struct Harness {
    root: PathBuf,
    source: PathBuf,
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
}
pub(super) fn state_root() -> Option<PathBuf> {
    HARNESS.with(|h| h.borrow().as_ref().map(|h| h.root.clone()))
}
pub(super) fn mapped_source(path: &Path) -> Option<PathBuf> {
    HARNESS.with(|h| {
        h.borrow().as_ref().and_then(|h| {
            (path == Path::new("/nix/store/original-source")).then(|| h.source.clone())
        })
    })
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
        std::fs::write(source.join("flake.nix"), "{ outputs = _: {}; }").unwrap();
        std::fs::write(source.join("flake.lock"), "{}").unwrap();
        let fake = Rc::new(RefCell::new(Fake::default()));
        HARNESS.with(|h| {
            *h.borrow_mut() = Some(Harness {
                root: root.clone(),
                source,
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
    let settings = json!({"activation":"switch","hostname":"node-a","sshUser":"root","sshPort":22,"identityFile":null,"autoRollback":true,"fastConnection":true});
    json!({"schemaVersion":1,"members":{"node-a":{"name":"node-a","deployment":{"deployRs":{"enable":true}}},"node-b":{"name":"node-b","deployment":{"deployRs":{"enable":true}}},"router":{"name":"router","deployment":{"deployRs":{"enable":false}}}},"groups":{"fleet":["node-a","node-b","router"]},"projections":{"deploy":{"nodes":["node-a","node-b"],"settings":{"node-a":settings,"node-b":settings}}}})
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
        ("flake", Some("metadata")) => json_bytes(
            json!({"path":if fake.move_source && fake.archives > 0 {"/nix/store/moved-source"} else {"/nix/store/original-source"},"revision":"revision-a","locks":locks()}),
        ),
        ("flake", Some("archive")) => {
            let moved = fake.archives > 0;
            fake.archives += 1;
            json_bytes(
                json!({"path":if fake.move_source && moved {"/nix/store/moved-source"} else {"/nix/store/original-source"},"inputs":{"local":{"path":if fake.move_input && moved {"/nix/store/moved-input"} else {"/nix/store/input-a"},"inputs":{}}}}),
            )
        }
        ("path-info", _) => json_bytes(
            json!({args[2].clone():{"narHash":format!("sha256-{}",args[2].rsplit('/').next().unwrap())}}),
        ),
        ("store", Some("add-path")) => {
            let path = args.last().unwrap();
            let lock: Value =
                serde_json::from_slice(&std::fs::read(Path::new(path).join("flake.lock"))?)
                    .map_err(invalid_json)?;
            assert_eq!(lock["nodes"]["local"]["locked"]["type"], "path");
            assert_eq!(
                lock["nodes"]["local"]["locked"]["path"],
                "/nix/store/input-a"
            );
            assert_eq!(
                lock["nodes"]["local"]["locked"]["narHash"],
                "sha256-input-a"
            );
            Ok(b"/nix/store/frozen-source\n".to_vec())
        }
        ("hash", Some("path")) => Ok(b"sha256-module-source\n".to_vec()),
        ("eval", _) if args.contains(&"--impure".into()) => {
            assert!(args.last().unwrap().starts_with("builtins.fetchTree"));
            json_bytes(json!({"outPath":"/nix/store/module-source"}))
        }
        ("eval", _) => {
            assert!(!args.contains(&"--impure".into()));
            let reference = args.last().unwrap();
            assert!(
                reference.starts_with("path:/nix/store/frozen-source#"),
                "must not evaluate a live source: {reference}"
            );
            if reference.ends_with("#mandala") {
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
            assert!(args.contains(&"--dry-run".into()), "capture cannot build");
            json_bytes(Value::Array(args.iter().filter(|arg| arg.contains("#deploy.nodes.")).map(|arg| {
                let name = if arg.contains(".node-a.") {"a"} else {"b"};
                json!({"drvPath":format!("/nix/store/profile-{name}.drv"),"outputs":{"out":if fake.unknown_output {Value::Null} else {Value::String(if fake.wrapper { format!("/nix/store/wrapper-{name}") } else { format!("/nix/store/system-{name}") })}}})
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
            .all(|(_, args)| args[0] != "eval" && args[0] != "build")
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
