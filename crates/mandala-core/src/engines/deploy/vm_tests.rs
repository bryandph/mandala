use super::*;
use crate::vm::owner::{GuestOutcome, Reply};
use crate::vm::tests::manifest;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
#[tokio::test]
async fn vm_owner_backend_native_owner_only_copy_activation_verify_order() {
    let directory = std::env::temp_dir().join(format!(
        "mandala-owner-native-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let profile = directory.join("profile");
    std::fs::create_dir_all(profile.join("mandala")).unwrap();
    let (manifest, unit, closure) = manifest();
    std::fs::write(
        profile.join(RELATIVE_PATH),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let trace = directory.join("trace");
    let response = |state, outcome| {
        serde_json::to_string(&Reply {
            version: 1,
            ok: true,
            error: None,
            journal: Some(Journal {
                version: 1,
                owner: "host-a".into(),
                transaction: "run-a".into(),
                state,
                unit: Some(unit.clone()),
                manifest: Some(manifest.clone()),
                profile: Some(profile.display().to_string()),
                previous: None,
                observed: None,
                guests: manifest
                    .guests
                    .keys()
                    .map(|n| (n.clone(), outcome))
                    .collect(),
                primary_failure: None,
                recovery_failure: None,
                recovery_limit: crate::vm::RECOVERY_LIMIT.into(),
            }),
        })
        .unwrap()
    };
    let handshake = serde_json::to_string(&Reply {
        version: 1,
        ok: true,
        error: None,
        journal: None,
    })
    .unwrap();
    let mut session = String::new();
    for (step, reply) in [
        ("hello", handshake),
        ("lock", response(State::Locked, GuestOutcome::Unknown)),
        ("prepare", response(State::Prepared, GuestOutcome::Unknown)),
        (
            "host-start",
            response(State::Mutating, GuestOutcome::Unknown),
        ),
        ("apply", response(State::Confirmed, GuestOutcome::Confirmed)),
        (
            "finish",
            response(State::Confirmed, GuestOutcome::Confirmed),
        ),
    ] {
        session.push_str(&format!("IFS= read -r request || exit 1\nprintf '%s\\n' helper-{step} >> '{}'\nprintf '%s\\n' '{}'\n",trace.display(), reply));
    }
    let ssh = directory.join("ssh");
    std::fs::write(&ssh, format!("#!/bin/sh\nprintf 'ssh %s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  *mandala-vm-owner*)\n{session}exit 0;;\n  *switch-inhibitors*) exit 1;;\n  *) exit 0;;\nesac\n",trace.display())).unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let nix = directory.join("nix");
    let paths: BTreeMap<_, _> = closure.into_iter().map(|p| (p, Value::Null)).collect();
    std::fs::write(&nix,format!("#!/bin/sh\nprintf 'nix %s\\n' \"$*\" >> '{}'\ncase \"$1\" in path-info) printf '%s\\n' '{}' ;; copy) exit 0;; *) exit 1;; esac\n",trace.display(),serde_json::to_string(&paths).unwrap())).unwrap();
    std::fs::set_permissions(&nix, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run_path = directory.join("run");
    std::fs::create_dir_all(&run_path).unwrap();
    let run = RegisteredDeployRun {
        run_id: "run-a".into(),
        path: run_path.clone(),
        plan: DeployPlan {
            limit: "guest-a".into(),
            targets: vec!["host-a".into()],
            skipped: vec![],
            settings: BTreeMap::from([(
                "host-a".into(),
                serde_json::json!({"activation":"switch","hostname":"owner.example.test","sshUser":"operator-alice","sshPort":2222,"autoRollback":true,"fastConnection":false,"magicRollback":false}),
            )]),
            vm: crate::vm::OwnerPlan {
                selected_members: vec!["guest-a".into()],
                requested_guests: vec!["guest-a".into()],
                units: BTreeMap::from([("host-a".into(), unit)]),
            },
            dry_activate: false,
            boot: false,
            halt_on_build_failure: false,
            publish: Default::default(),
            throttle: 1,
        },
    };
    let built = BuiltProfiles {
        paths: BTreeMap::from([("host-a".into(), profile)]),
        failures: BTreeMap::new(),
    };
    let result = super::deploy_host_with(
        &run,
        &built,
        "host-a",
        &DeployPrograms {
            nix: Some(nix),
            ssh: Some(ssh),
            environment: vec![],
        },
    )
    .await;
    let trace = std::fs::read_to_string(&trace).unwrap();
    assert_eq!(result.state, HostState::Confirmed, "{result:?}\n{trace}");
    let positions: Vec<_> = [
        "helper-lock",
        "nix copy",
        "helper-prepare",
        "helper-host-start",
        "activate-rs",
        "helper-apply",
        "helper-finish",
    ]
    .iter()
    .map(|step| {
        trace
            .find(step)
            .unwrap_or_else(|| panic!("missing {step}: {trace}"))
    })
    .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "{trace}"
    );
    assert!(!trace.contains("nix build") && !trace.contains("nix eval"));
    assert!(
        trace
            .lines()
            .filter(|line| line.starts_with("ssh "))
            .all(|line| line.contains("operator-alice@owner.example.test"))
    );
    let journal: Journal =
        serde_json::from_slice(&std::fs::read(run_path.join("vm/host-a.json")).unwrap()).unwrap();
    assert_eq!(journal.state, State::Confirmed);
    assert!(
        journal
            .guests
            .values()
            .all(|s| *s == GuestOutcome::Confirmed)
    );
    assert!(!run_path.join("guest-a.jsonl").exists() && !run_path.join("guest-b.jsonl").exists());
    std::fs::remove_dir_all(directory).unwrap();
}
