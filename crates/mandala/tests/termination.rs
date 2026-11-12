//! Real engine/supervisor processes with shell-only build/copy/ssh effects.
//! Signals target only children spawned by this fixture, never the live fleet.
use mandala_core::{DeployRun, registry};
use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn signal(pid: u32, name: &str) {
    if !registry::pid_alive(Some(i64::from(pid))) {
        return;
    }
    let _ = Command::new("sh")
        .args(["-c", "kill -s \"$1\" \"$2\"", "sh", name, &pid.to_string()])
        .status();
}

fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(12);
    while !condition() {
        assert!(Instant::now() < deadline, "fixture condition timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn program(path: &Path, text: &str) {
    // Close the writer before any parallel test forks an executable fixture.
    let mut child = Command::new("sh")
        .args(["-c", "cat > \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

struct Fixture {
    root: PathBuf,
    child: Child,
}

impl Fixture {
    fn start(phase: &str, supervised: bool) -> Self {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "mandala-terminate-{phase}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("bin")).unwrap();
        let setting = |host: &str| {
            json!({
                "activation":"switch", "hostname":format!("{host}.invalid"), "sshUser":"deployer", "sshPort":22,
                "sshOpts":[], "autoRollback":phase != "no-auto-rollback", "fastConnection":false, "magicRollback":true,
                "confirmTimeout":30, "activationTimeout":if phase == "wait-failure" { 1 } else { 60 }, "tempPath":"/tmp/mandala-test", "sudo":null, "user":"root"
            })
        };
        fs::write(root.join("aggregate.json"), json!({
            "schemaVersion":1,
            "members":{"cache":{"name":"cache","deployment":{"deployRs":{"enable":true}}}, "web":{"name":"web","deployment":{"deployRs":{"enable":true}}}},
            "groups":{"fleet":["cache","web"]},
            "projections":{"deploy":{"nodes":["cache","web"], "settings":{"cache":setting("cache"),"web":setting("web")}}}
        }).to_string()).unwrap();
        program(
            &root.join("bin/nix"),
            r#"#!/bin/sh
set -eu
block() {
  echo $$ > "$MANDALA_FLEET_STATE/$1.pid"
  sh -c 'echo $$ > "$1"; exec sleep 60' sh "$MANDALA_FLEET_STATE/$1-leaf.pid" &
  leaf=$!
  trap 'kill -TERM "$leaf" 2>/dev/null || :; wait "$leaf" 2>/dev/null || :; exit 143' TERM INT
  wait "$leaf"
}
case "$1" in
 build)
  case " $* " in
   *' --dry-run '*) printf '%s\n' '[{"drvPath":"/nix/store/00000000000000000000000000000000-cache-profile.drv","outputs":{"out":"/nix/store/00000000000000000000000000000000-cache-profile"}},{"drvPath":"/nix/store/11111111111111111111111111111111-web-profile.drv","outputs":{"out":"/nix/store/11111111111111111111111111111111-web-profile"}}]'; exit 0 ;;
   *' --offline '*) exit 1 ;;
  esac
  if [ "$TEST_PHASE" = build ]; then block build; fi
  if [ "$TEST_PHASE" = stubborn-build ]; then
    trap '' TERM INT
    echo $$ > "$MANDALA_FLEET_STATE/build.pid"
    exec sleep 60
  fi
  link=
  while [ "$#" -gt 0 ]; do
   if [ "$1" = --out-link ]; then link=$2; shift 2; else shift; fi
  done
  ln -s /nix/store/00000000000000000000000000000000-cache-profile "$link"
  ln -s /nix/store/11111111111111111111111111111111-web-profile "$link-1"
  ;;
 copy)
  case "$TEST_PHASE" in
   publish) block publish ;;
   stubborn-publish) trap '' TERM INT; echo $$ > "$MANDALA_FLEET_STATE/publish.pid"; exec sleep 60 ;;
  esac
  touch "$MANDALA_FLEET_STATE/host-copy-started"
  case "$TEST_PHASE:$*" in fanout:*cache.invalid*) block copy ;; esac
  ;;
 *) exit 90 ;;
esac
"#,
        );
        program(
            &root.join("bin/ssh"),
            r#"#!/bin/sh
set -eu
if [ "$TEST_PHASE" = fanout ]; then
  echo $$ > "$MANDALA_FLEET_STATE/ssh.pid"
  sh -c 'echo $$ > "$1"; exec sleep 60' sh "$MANDALA_FLEET_STATE/ssh-leaf.pid" &
  leaf=$!
  trap 'kill -TERM "$leaf" 2>/dev/null || :; wait "$leaf" 2>/dev/null || :; exit 143' TERM INT
  wait "$leaf"
fi
host=web
case "$*" in *cache.invalid*) host=cache ;; esac
case "$*" in
 *'test -e '*) exit 1 ;;
 *'rm -f '*) exit 0 ;;
 *'activate-rs activate '*)
  echo $$ > "$MANDALA_FLEET_STATE/activate-$host.pid"
  # Model activate-rs 1.0: signal cancellation before rollback finishes.
  touch "$MANDALA_FLEET_STATE/cancel-$host"
  if [ "$TEST_PHASE" = rollback ] || [ "$TEST_PHASE" = no-auto-rollback ]; then
    while [ ! -f "$MANDALA_FLEET_STATE/waited-$host" ]; do sleep 0.02; done
    sleep 0.2
    echo "rollback-output-$host"
    echo "rollback-stderr-$host" >&2
    touch "$MANDALA_FLEET_STATE/finished-$host"
    exit 1
  fi
  sh -c 'echo $$ > "$1"; exec sleep 60' sh "$MANDALA_FLEET_STATE/activate-$host-leaf.pid" &
  wait ;;
 *'activate-rs wait '*)
  while [ ! -f "$MANDALA_FLEET_STATE/cancel-$host" ]; do sleep 0.02; done
  touch "$MANDALA_FLEET_STATE/waited-$host"
  exit 1 ;;
 *) exit 0 ;;
esac
"#,
        );
        let argv = vec![
            env!("CARGO_BIN_EXE_mandala").to_string(),
            "--flake".into(),
            root.to_string_lossy().into_owned(),
            "deploy".into(),
            "run".into(),
            "--limit".into(),
            "@fleet".into(),
        ];
        let mut command = if supervised {
            let launch = root.join("deploy-launches/fixture");
            fs::create_dir_all(&launch).unwrap();
            fs::write(launch.join("output.log"), "").unwrap();
            fs::write(
                launch.join("supervisor.json"),
                json!({"argv":argv,"cwd":root}).to_string(),
            )
            .unwrap();
            let mut command = Command::new(env!("CARGO_BIN_EXE_mandala-run-supervisor"));
            command.arg(launch);
            command
        } else {
            let mut command = Command::new(&argv[0]);
            command.args(&argv[1..]);
            command
        };
        let path = std::env::join_paths(
            std::iter::once(root.join("bin"))
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let log = fs::File::create(root.join("engine.log")).unwrap();
        if phase.contains("publish") {
            command.env("MANDALA_CACHE_URL", "file:///fictional-cache");
        } else {
            command.env_remove("MANDALA_CACHE_URL");
        }
        let child = command
            .env_remove("MANDALA_CACHE_NETRC_FILE")
            .env("PATH", path)
            .env("TEST_PHASE", phase)
            .env("MANDALA_FLEET_STATE", &root)
            .env("MANDALA_AGGREGATE_FILE", root.join("aggregate.json"))
            .current_dir(&root)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        Self { root, child }
    }

    fn pid(&self, label: &str) -> u32 {
        let path = self.root.join(format!("{label}.pid"));
        until(|| path.exists());
        let mut pid = None;
        until(|| {
            pid = fs::read_to_string(&path)
                .ok()
                .and_then(|s| s.trim().parse().ok());
            pid.is_some()
        });
        pid.unwrap()
    }

    fn run_dir(&self) -> PathBuf {
        let mut dir = None;
        until(|| {
            dir = fs::read_dir(self.root.join("runs"))
                .ok()
                .and_then(|mut entries| entries.next())
                .and_then(Result::ok)
                .map(|entry| entry.path());
            dir.is_some()
        });
        dir.unwrap()
    }

    fn assert_settled(&mut self, pids: &[u32]) {
        let mut status = None;
        until(|| {
            status = self.child.try_wait().unwrap();
            status.is_some()
        });
        assert!(!status.unwrap().success());
        until(|| {
            pids.iter()
                .all(|pid| !registry::pid_alive(Some(i64::from(*pid))))
        });
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            signal(self.child.id(), "TERM");
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        // Only fixture-recorded effect PIDs; this also cleans up on assertion failure.
        if let Ok(entries) = fs::read_dir(&self.root) {
            for entry in entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "pid"))
            {
                if let Ok(text) = fs::read_to_string(entry.path())
                    && let Ok(pid) = text.trim().parse()
                {
                    signal(pid, "TERM");
                }
            }
        }
    }
}

#[test]
fn terminate_during_publish_forwards_both_signals_without_fallback() {
    for signal_name in ["TERM", "INT"] {
        let mut fixture = Fixture::start("publish", true);
        let pids = [fixture.pid("publish"), fixture.pid("publish-leaf")];
        signal(fixture.child.id(), signal_name);
        fixture.assert_settled(&pids);
        assert!(!fixture.root.join("host-copy-started").exists());
        let meta = registry::read_meta(&fixture.run_dir());
        assert_eq!(meta["publish"]["outcome"], "interrupted");
        assert_ne!(meta["rc"], 0);
    }
}

#[test]
fn publish_escalates_ignoring_child_without_starting_host_copy() {
    let mut fixture = Fixture::start("stubborn-publish", false);
    let pids = [fixture.pid("publish")];
    signal(fixture.child.id(), "INT");
    fixture.assert_settled(&pids);
    assert!(!fixture.root.join("host-copy-started").exists());
}

#[test]
fn terminate_during_build_leaves_no_build_child_or_descendant() {
    let mut fixture = Fixture::start("build", false);
    let pids = [fixture.pid("build"), fixture.pid("build-leaf")];
    let mut run = DeployRun::new("@fleet");
    run.events_dir = Some(fixture.run_dir());
    run.terminate();
    fixture.assert_settled(&pids);
}

#[test]
fn terminate_during_fanout_leaves_no_copy_or_ssh_children() {
    let mut fixture = Fixture::start("fanout", false);
    let pids = [
        fixture.pid("copy"),
        fixture.pid("copy-leaf"),
        fixture.pid("ssh"),
        fixture.pid("ssh-leaf"),
    ];
    let mut run = DeployRun::new("@fleet");
    run.events_dir = Some(fixture.run_dir());
    run.terminate();
    fixture.assert_settled(&pids);
}

#[test]
fn supervisor_forwards_sigterm_and_sigint_to_engine_children() {
    for name in ["TERM", "INT"] {
        let mut fixture = Fixture::start("build", true);
        let pids = [fixture.pid("build"), fixture.pid("build-leaf")];
        signal(fixture.child.id(), name);
        fixture.assert_settled(&pids);
    }
}

#[test]
fn engine_escalates_when_build_ignores_sigint() {
    let mut fixture = Fixture::start("stubborn-build", false);
    let pids = [fixture.pid("build")];
    signal(fixture.child.id(), "INT");
    fixture.assert_settled(&pids);
}

#[test]
fn failed_waiter_bounds_activation_wait_and_kills_owned_group() {
    let mut fixture = Fixture::start("wait-failure", false);
    let pids = [
        fixture.pid("activate-cache"),
        fixture.pid("activate-web"),
        fixture.pid("activate-cache-leaf"),
        fixture.pid("activate-web-leaf"),
    ];
    fixture.assert_settled(&pids);
    let log = fs::read_to_string(fixture.root.join("engine.log")).unwrap();
    assert!(log.contains("activation did not finish within 1s"), "{log}");
}

#[test]
fn terminate_during_post_waiter_rollback_wait_kills_activate_group() {
    let mut fixture = Fixture::start("terminate-rollback", false);
    let pids = [
        fixture.pid("activate-cache"),
        fixture.pid("activate-web"),
        fixture.pid("activate-cache-leaf"),
        fixture.pid("activate-web-leaf"),
    ];
    let run_dir = fixture.run_dir();
    until(|| {
        ["cache", "web"].iter().all(|host| {
            fs::read_to_string(run_dir.join(format!("{host}.jsonl")))
                .unwrap_or_default()
                .contains("activation waiter failed; waiting")
        })
    });
    let mut run = DeployRun::new("@fleet");
    run.events_dir = Some(fixture.run_dir());
    run.terminate();
    fixture.assert_settled(&pids);
}

#[test]
fn cancel_before_rollback_completion_preserves_output_and_terminal_state() {
    for (phase, expected) in [("rollback", "rolled_back"), ("no-auto-rollback", "failed")] {
        let mut fixture = Fixture::start(phase, false);
        let pids = [fixture.pid("activate-cache"), fixture.pid("activate-web")];
        fixture.assert_settled(&pids);
        for host in ["cache", "web"] {
            assert!(fixture.root.join(format!("finished-{host}")).exists());
            let events =
                fs::read_to_string(fixture.run_dir().join(format!("{host}.jsonl"))).unwrap();
            assert!(
                events.contains(&format!("rollback-output-{host}")),
                "{events}"
            );
            assert!(
                events.contains(&format!("rollback-stderr-{host}")),
                "{events}"
            );
        }
        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(fixture.run_dir().join("meta.json")).unwrap())
                .unwrap();
        assert_eq!(meta["summary"][expected], 2, "{meta}");
    }
}
