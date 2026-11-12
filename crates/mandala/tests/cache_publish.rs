//! Real native engine with shell-only Nix/SSH effects; never a live cache.
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "mandala-publish-engine-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("bin")).unwrap();
        let settings = |host: &str| json!({"activation":"switch","hostname":format!("{host}.acme.example"),"sshUser":"deployer","sshPort":22,"sshOpts":[],"autoRollback":true,"fastConnection":host == "b","magicRollback":true,"confirmTimeout":30,"activationTimeout":60,"tempPath":"/tmp/mandala-test","sudo":null,"user":"root"});
        fs::write(root.join("aggregate.json"), json!({"schemaVersion":1,"members":{"a":{"name":"a","deployment":{"deployRs":{"enable":true}}},"b":{"name":"b","deployment":{"deployRs":{"enable":true}}}},"groups":{"fleet":["a","b"]},"projections":{"deploy":{"nodes":["a","b"],"settings":{"a":settings("a"),"b":settings("b")}}}}).to_string()).unwrap();
        let this = Self(root);
        this.program("nix", r#"#!/bin/sh
set -eu
printf '%s|%s|netrc=%s\n' "$1" "$*" "${MANDALA_CACHE_NETRC_FILE-unset}" >> "$MANDALA_FLEET_STATE/trace"
case "$1" in
 build)
  case " $* " in
   *' --dry-run '*)
    if [ "${TEST_PARTIAL-0}" = 1 ]; then second=bbbb; else second=aaaa; fi
    printf '[{"drvPath":"/nix/store/aaaa-profile.drv","outputs":{"out":"/nix/store/aaaa-profile"}},{"drvPath":"/nix/store/%s-profile.drv","outputs":{"out":"/nix/store/%s-profile"}}]\n' "$second" "$second"
    exit 0 ;;
   *' --offline '*)
    case "$2" in *bbbb*) exit 1 ;; esac
    link=; while [ "$#" -gt 0 ]; do if [ "$1" = --out-link ]; then link=$2; shift 2; else shift; fi; done
    ln -s /nix/store/aaaa-profile "$link"; exit 0 ;;
  esac
  [ "${TEST_ABORT-0}" != 1 ] || exit 130
  [ "${TEST_PARTIAL-0}" != 1 ] || exit 23
  link=; while [ "$#" -gt 0 ]; do if [ "$1" = --out-link ]; then link=$2; shift 2; else shift; fi; done
  ln -s /nix/store/aaaa-profile "$link"
  ln -s /nix/store/aaaa-profile "$link-1" ;;
 copy)
  case "$*" in
   *ssh://*) test -f "$MANDALA_FLEET_STATE/published" || [ "${TEST_DISABLED-0}" = 1 ] ;;
   *) touch "$MANDALA_FLEET_STATE/published"; printf 'backend-sensitive-output netrc-secret token-private\n'; printf 'backend-sensitive-output\n' >&2; exit "${TEST_PUBLISH_RC-0}" ;;
  esac ;;
 *) exit 90 ;;
esac
"#);
        this.program("ssh", "#!/bin/sh\nset -eu\nprintf 'ssh|%s|netrc=%s\\n' \"$*\" \"${MANDALA_CACHE_NETRC_FILE-unset}\" >> \"$MANDALA_FLEET_STATE/trace\"\ncase \"$*\" in *'readlink -e '*) echo /nix/store/old-system; exit 0 ;; *'test -e '*) exit 1 ;; esac\nexit 0\n");
        this
    }
    fn program(&self, name: &str, text: &str) {
        let path = self.0.join("bin").join(name);
        fs::write(&path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn command(&self) -> Command {
        self.command_mode(true)
    }
    fn command_mode(&self, dry: bool) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mandala"));
        command
            .args(["deploy", "run", "--limit", "@fleet"])
            .env("MANDALA_AGGREGATE_FILE", self.0.join("aggregate.json"))
            .env("MANDALA_FLEET_STATE", &self.0)
            .env(
                "MANDALA_CACHE_URL",
                "https://operator:token-private@cache.acme.example?auth=token-private#private",
            )
            .env(
                "MANDALA_CACHE_NETRC_FILE",
                self.0.join("nonexistent netrc-secret"),
            )
            .env(
                "PATH",
                std::env::join_paths(
                    std::iter::once(self.0.join("bin"))
                        .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
                )
                .unwrap(),
            )
            .current_dir(&self.0);
        if dry {
            command.arg("--dry-activate");
        }
        command
    }
    fn meta(&self) -> Value {
        let path = fs::read_dir(self.0.join("runs"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        serde_json::from_str(&fs::read_to_string(path.join("meta.json")).unwrap()).unwrap()
    }
    fn trace(&self) -> String {
        fs::read_to_string(self.0.join("trace")).unwrap()
    }
    fn assert_redacted(&self, output: &Output) {
        let mut text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        for entry in fs::read_dir(self.0.join("runs")).unwrap() {
            for file in fs::read_dir(entry.unwrap().path()).unwrap() {
                let file = file.unwrap().path();
                if file
                    .extension()
                    .is_some_and(|e| e == "json" || e == "jsonl")
                {
                    text.push_str(&fs::read_to_string(file).unwrap());
                }
            }
        }
        for sensitive in ["token-private", "netrc-secret", "backend-sensitive-output"] {
            assert!(!text.contains(sensitive), "leaked {sensitive}: {text}");
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn native_union_barrier_fallback_redaction_and_dry_boot() {
    for rc in [0, 23] {
        let f = Fixture::new();
        let output = f
            .command()
            .arg("--boot")
            .env("TEST_PUBLISH_RC", rc.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let trace = f.trace();
        let lines: Vec<_> = trace.lines().collect();
        let uploads: Vec<_> = lines
            .iter()
            .filter(|line| line.starts_with("copy|") && !line.contains("ssh://"))
            .collect();
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].matches("/nix/store/aaaa-profile").count(), 1);
        assert!(uploads[0].contains("--option netrc-file"));
        assert!(lines.iter().all(|line| line.ends_with("netrc=unset")));
        let upload_index = lines.iter().position(|line| line == uploads[0]).unwrap();
        assert!(
            lines[..upload_index]
                .iter()
                .all(|line| line.starts_with("build|"))
        );
        assert!(
            lines[upload_index + 1..]
                .iter()
                .any(|line| line.contains("--substitute-on-destination"))
        );
        assert!(
            lines[upload_index + 1..]
                .iter()
                .filter(|line| line.starts_with("copy|"))
                .all(|line| line.contains("ssh://"))
        );
        let fast = lines
            .iter()
            .find(|line| line.starts_with("copy|") && line.contains("@b.acme.example"))
            .unwrap();
        assert!(!fast.contains("--substitute-on-destination"));
        assert!(!trace.contains("activate-rs activate"));
        assert!(!trace.contains("switch-to-configuration boot"));
        let meta = f.meta();
        assert_eq!(
            meta["publish"]["outcome"],
            if rc == 0 { "succeeded" } else { "failed" }
        );
        assert_eq!(meta["publish"]["path_count"], 1);
        assert_eq!(meta["rc"], 0);
        assert_eq!(meta["boot"], true);
        f.assert_redacted(&output);
    }
}

#[test]
fn publish_precedes_real_boot_staging_and_mixed_member_modes() {
    for boot_override in [false, true] {
        let f = Fixture::new();
        let path = f.0.join("aggregate.json");
        let mut aggregate: Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        aggregate["projections"]["deploy"]["settings"]["b"]["activation"] = json!("boot");
        fs::write(&path, aggregate.to_string()).unwrap();
        let mut command = f.command_mode(false);
        if boot_override {
            command.arg("--boot");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let meta = f.meta();
        assert_eq!(meta["publish"]["outcome"], "succeeded");
        assert_eq!(
            meta["summary"]["reboot_pending"],
            if boot_override { 2 } else { 1 }
        );
        assert_eq!(
            meta["summary"]["confirmed"],
            if boot_override { 0 } else { 1 }
        );
        let trace = f.trace();
        assert!(trace.contains("switch-to-configuration boot"));
        assert!(trace.find("copy|").unwrap() < trace.find("ssh|").unwrap());
        f.assert_redacted(&output);
    }
}

#[test]
fn recovery_uploads_only_verified_outputs_and_retains_failed_hosts() {
    let f = Fixture::new();
    let output = f.command().env("TEST_PARTIAL", "1").output().unwrap();
    assert!(!output.status.success());
    let trace = f.trace();
    let upload = trace
        .lines()
        .find(|line| line.starts_with("copy|") && !line.contains("ssh://"))
        .unwrap();
    assert!(upload.contains("/nix/store/aaaa-profile"));
    assert!(!upload.contains("bbbb"));
    let probe = trace.find("--offline").unwrap();
    assert!(probe < trace.find("copy|").unwrap());
    assert!(trace.contains("--max-jobs 0 --builders  --option substitute false"));
    assert!(
        !trace
            .lines()
            .any(|line| line.starts_with("copy|") && line.contains("@b.acme.example"))
    );
    assert_eq!(f.meta()["summary"]["failed"], 1);
    f.assert_redacted(&output);
}

#[test]
fn forced_missing_url_preflight_and_disable_override() {
    let f = Fixture::new();
    let output = f
        .command()
        .arg("--cache-push")
        .env_remove("MANDALA_CACHE_URL")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!f.0.join("runs").exists());
    assert!(!f.0.join("trace").exists());
    let output = f
        .command()
        .arg("--no-cache-push")
        .env("MANDALA_CACHE_URL", "malformed secret input")
        .env("TEST_DISABLED", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(f.meta()["publish"]["reason"], "disabled");
    assert!(!f.0.join("published").exists());
}

#[test]
fn aborted_and_halt_on_failure_builds_never_publish() {
    for aborted in [false, true] {
        let f = Fixture::new();
        let mut command = f.command();
        if aborted {
            command.env("TEST_ABORT", "1");
        } else {
            command
                .env("TEST_PARTIAL", "1")
                .arg("--halt-on-build-failure");
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(!f.0.join("published").exists());
        assert!(!f.trace().contains("copy|"));
        assert_eq!(f.meta()["publish"]["reason"], "build-not-completed");
    }
}
