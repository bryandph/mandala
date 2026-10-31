//! Production DeployRun::start -> supervisor -> native engine metadata, with
//! shell-only build effects in a private registry. No live fleet operations.
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mandala_core::{DeployRun, RunLiveness, registry};
use serde_json::json;

fn signal(pid: i64, name: &str) {
    if registry::pid_alive(Some(pid)) {
        assert!(
            Command::new("sh")
                .args(["-c", "kill -s \"$1\" \"$2\"", "sh", name, &pid.to_string()])
                .status()
                .unwrap()
                .success()
        );
    }
}

async fn until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(12);
    while !ready() {
        assert!(Instant::now() < deadline, "fixture condition timed out");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn program(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn setup() -> PathBuf {
    let root = std::env::temp_dir().join(format!("mandala-supervisor-meta-{}", std::process::id()));
    fs::create_dir_all(root.join("bin")).unwrap();
    let setting = json!({"activation":"switch", "hostname":"stub.invalid", "sshUser":"deployer", "sshPort":22,
        "sshOpts":[], "autoRollback":true,"fastConnection":false,"magicRollback":true,"confirmTimeout":30,
        "activationTimeout":60,"tempPath":"/tmp/mandala-test","sudo":null,"user":"root"});
    fs::write(root.join("aggregate.json"), json!({"schemaVersion":1,
        "members":{"web":{"name":"web","deployment":{"deployRs":{"enable":true}}}},
        "groups":{"fleet":["web"]},"projections":{"deploy":{"nodes":["web"],"settings":{"web":setting}}}
    }).to_string()).unwrap();
    program(
        &root.join("bin/nix"),
        r#"#!/bin/sh
set -eu
if [ "${MANDALA_DEPLOY_SUPERVISOR_PID+x}" = x ]; then
  echo leaked > "$MANDALA_FLEET_STATE/handoff-leaked"
  exit 98
fi
echo clear >> "$MANDALA_FLEET_STATE/handoff-cleared"
case " $* " in
 *' --dry-run '*)
  printf '%s\n' '[{"drvPath":"/nix/store/00000000000000000000000000000000-web-profile.drv","outputs":{"out":"/nix/store/00000000000000000000000000000000-web-profile"}}]'
  exit 0 ;;
esac
printf '%s\n' $$ > "$MANDALA_FLEET_STATE/build.pid"
exec sleep 60
"#,
    );
    program(&root.join("bin/ssh"), "#!/bin/sh\nexit 99\n");
    // Exec preserves the PID, exposing the identity of our own supervisor
    // without relying on ps/procfs or adding a production PID accessor.
    program(
        &root.join("bin/supervisor"),
        &format!(
            "#!/bin/sh\necho $$ > \"$MANDALA_FLEET_STATE/supervisor.pid\"\nexec '{}' \"$@\"\n",
            env!("CARGO_BIN_EXE_mandala-run-supervisor")
        ),
    );
    let path = std::env::join_paths(
        std::iter::once(root.join("bin"))
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    // SAFETY: one test owns this binary's environment; initialize before the
    // current-thread runtime or any subprocess is started.
    unsafe {
        std::env::set_var("MANDALA_FLEET_STATE", &root);
        std::env::set_var("MANDALA_AGGREGATE_FILE", root.join("aggregate.json"));
        std::env::set_var("MANDALA_DEPLOY_ENGINE_BIN", env!("CARGO_BIN_EXE_mandala"));
        std::env::set_var("MANDALA_DEPLOY_SUPERVISOR_BIN", root.join("bin/supervisor"));
        std::env::set_var("PATH", path);
        std::env::remove_var("MANDALA_DEPLOY_SUPERVISOR_PID");
    }
    root
}

struct RunningDeploy(DeployRun);
impl Drop for RunningDeploy {
    fn drop(&mut self) {
        // Explicit fixture cleanup, including the deliberately orphaned engine
        // after killing its supervisor. Ordinary frontend Drop still detaches.
        self.0.terminate();
        if let Some(path) = &self.0.events_dir {
            let meta = registry::read_meta(path);
            if let Some(pid) = meta.get("pid").and_then(|v| v.as_i64()) {
                signal(pid, "TERM");
            }
        }
    }
}

/// A FIFO is the existing inventory-file seam: opening its writer proves the
/// engine reached inventory loading, after main captured/cleared the handoff,
/// but before it can register a run. No timing-only delay or production hook.
async fn supervisor_death_during_inventory_read_preserves_early_identity(root: &Path) {
    let fifo = root.join("inventory.fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    fs::remove_file(root.join("supervisor.pid")).unwrap();
    let contents = fs::read(root.join("aggregate.json")).unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let writer_path = fifo.clone();
    let writer = std::thread::spawn(move || {
        use std::io::Write;
        let mut pipe = fs::OpenOptions::new()
            .write(true)
            .open(writer_path)
            .unwrap();
        ready_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        pipe.write_all(&contents).unwrap();
    });
    let mut early = RunningDeploy(DeployRun::new("web"));
    // env execs the actual engine without changing its parent. The production
    // start/supervisor path still performs the private PID handoff itself.
    early.0.program = Some(vec![
        "env".into(),
        format!("MANDALA_AGGREGATE_FILE={}", fifo.display()),
        env!("CARGO_BIN_EXE_mandala").into(),
        "--flake".into(),
        root.to_string_lossy().into_owned(),
        "deploy".into(),
        "run".into(),
        "--limit".into(),
        "web".into(),
    ]);
    early.0.start().await.unwrap();
    until(|| ready_rx.try_recv().is_ok()).await;
    let supervisor = fs::read_to_string(root.join("supervisor.pid"))
        .unwrap()
        .trim()
        .parse::<i64>()
        .unwrap();
    signal(supervisor, "KILL");
    until(|| early.0.finished()).await; // supervisor reaped before registration
    release_tx.send(()).unwrap();
    writer.join().unwrap();
    until(|| {
        early
            .0
            .output()
            .iter()
            .any(|line| registry::is_valid_run_id(line.trim()))
    })
    .await;
    assert!(early.0.discover_run().unwrap());
    let meta = registry::read_meta(early.0.events_dir.as_ref().unwrap());
    assert_eq!(
        meta["supervisor_pid"].as_i64(),
        Some(supervisor),
        "preserve validated early parent after reparenting"
    );
    assert!(!root.join("handoff-leaked").exists());
    let mut observed = registry::open_run(early.0.run_id.as_ref().unwrap()).unwrap();
    assert_eq!(observed.liveness(), RunLiveness::Failed);
    signal(meta["pid"].as_i64().unwrap(), "TERM");
}

#[test]
fn production_supervisor_identity_drives_orphan_detection_and_retention() {
    let root = setup();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut owned = RunningDeploy(DeployRun::new("@fleet"));
        owned.0.flake = root.to_string_lossy().into_owned();
        owned.0.start().await.unwrap();
        until(|| owned.0.discover_run().unwrap()).await;
        until(|| root.join("build.pid").exists()).await;
        let run_id = owned.0.run_id.as_ref().unwrap().clone();
        let run_dir = owned.0.events_dir.as_ref().unwrap().clone();
        let meta = registry::read_meta(&run_dir);
        let engine = meta["pid"].as_i64().unwrap();
        let supervisor = meta["supervisor_pid"]
            .as_i64()
            .expect("production registration writes supervisor_pid");
        assert_eq!(
            fs::read_to_string(root.join("supervisor.pid"))
                .unwrap()
                .trim()
                .parse::<i64>()
                .unwrap(),
            supervisor
        );
        assert!(root.join("handoff-cleared").exists());
        assert!(
            !root.join("handoff-leaked").exists(),
            "engine effects must not inherit the handoff"
        );
        assert_ne!(engine, supervisor);
        assert_ne!(supervisor, i64::from(std::process::id()));
        assert!(registry::pid_alive(Some(supervisor)));
        assert!(!meta.contains_key("rc"));
        let mut observed = registry::open_run(&run_id).unwrap();
        let mut attached = DeployRun::attach(&run_id).unwrap();
        assert_eq!(observed.liveness(), RunLiveness::Running);
        assert!(!attached.finished());
        // Exercise retention against actual registered metadata, not fake keys.
        registry::prune(Some(0));
        assert!(run_dir.exists(), "active production run survives pruning");
        signal(supervisor, "KILL");
        until(|| owned.0.finished()).await; // reap supervisor, avoiding zombie-as-live
        assert!(!registry::pid_alive(Some(supervisor)));
        assert!(
            registry::pid_alive(Some(engine)),
            "engine deliberately remains alive for orphan detection"
        );
        assert!(!registry::read_meta(&run_dir).contains_key("rc"));
        assert_eq!(observed.liveness(), RunLiveness::Failed);
        assert!(attached.finished());
        assert_eq!(attached.returncode(), Some(1));
        let build = fs::read_to_string(root.join("build.pid"))
            .unwrap()
            .trim()
            .parse::<i64>()
            .unwrap();
        signal(engine, "TERM");
        until(|| !registry::pid_alive(Some(build))).await;

        supervisor_death_during_inventory_read_preserves_early_identity(&root).await;

        // Direct CLI runs omit the field. Malformed and inherited/mismatched
        // hints (including the already-dead supervisor from another run) must
        // not make a healthy independently launched engine look orphaned.
        for hint in [
            None,
            Some("not-a-pid".to_owned()),
            Some(supervisor.to_string()),
        ] {
            let log = fs::File::create(root.join("direct.stderr")).unwrap();
            let mut command = Command::new(env!("CARGO_BIN_EXE_mandala"));
            command
                .args([
                    "--flake",
                    root.to_str().unwrap(),
                    "deploy",
                    "run",
                    "--limit",
                    "web",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(log);
            if let Some(hint) = &hint {
                command.env("MANDALA_DEPLOY_SUPERVISOR_PID", hint);
            } else {
                command.env_remove("MANDALA_DEPLOY_SUPERVISOR_PID");
            }
            let mut child = command.spawn().unwrap();
            let mut id = String::new();
            BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut id)
                .unwrap();
            assert!(registry::is_valid_run_id(id.trim()));
            let path = registry::runs_dir().join(id.trim());
            let meta = registry::read_meta(&path);
            assert!(!meta.contains_key("supervisor_pid"));
            let mut independent = registry::open_run(id.trim()).unwrap();
            assert_eq!(independent.liveness(), RunLiveness::Running);
            signal(i64::from(child.id()), "TERM");
            until(|| child.try_wait().unwrap().is_some()).await;
        }
    });
}
