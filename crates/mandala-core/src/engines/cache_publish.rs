//! Optional, credential-agnostic publication of verified native deploy profiles.
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::PathBuf;
use std::process::Command;

use clap::{Arg, ArgAction, ArgMatches};
use serde_json::{Value, json};

use super::deploy::{BuiltProfiles, RegisteredDeployRun};
use crate::registry::{self, Meta};
use crate::runner::EventWriter;

/// None means auto; Some(true/false) explicitly enables/disables publication.
pub fn flags() -> [Arg; 2] {
    [
        Arg::new("cache-push")
            .long("cache-push")
            .action(ArgAction::SetTrue)
            .conflicts_with("no-cache-push")
            .help("Publish built profiles to MANDALA_CACHE_URL before fan-out"),
        Arg::new("no-cache-push")
            .long("no-cache-push")
            .action(ArgAction::SetTrue)
            .help("Disable cache publication even when configured"),
    ]
}

#[must_use]
pub fn choice(matches: &ArgMatches) -> Option<bool> {
    if matches.get_flag("no-cache-push") {
        Some(false)
    } else if matches.get_flag("cache-push") {
        Some(true)
    } else {
        None
    }
}

/// Raw upload arguments are deliberately not Debug/Serialize: they can contain credentials.
#[derive(Clone, Default, PartialEq)]
pub struct Config {
    url: Option<OsString>,
    netrc: Option<OsString>,
    safe_url: Option<String>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachePublish")
            .field("cache_url", &self.safe_url)
            .finish_non_exhaustive()
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "invalid cache URL")
}

/// Validate store URI syntax without restricting Nix's backend schemes. Backend
/// availability is Nix's decision. Never include raw input in an error.
fn safe_url(raw: &str) -> io::Result<String> {
    if raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid());
    }
    let clean = raw.split(['?', '#']).next().ok_or_else(invalid)?;
    if let Some((scheme, rest)) = clean.split_once(':') {
        if !scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            || !scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
            || rest.is_empty()
        {
            return Err(invalid());
        }
        if let Some(authority_path) = rest.strip_prefix("//") {
            let (authority, path) = authority_path
                .split_once('/')
                .unwrap_or((authority_path, ""));
            let authority = authority.rsplit('@').next().ok_or_else(invalid)?;
            if authority.is_empty() && scheme != "file" {
                return Err(invalid());
            }
            return Ok(format!(
                "{scheme}://{authority}{}{}",
                if authority_path.contains('/') {
                    "/"
                } else {
                    ""
                },
                path
            ));
        }
        return Ok(clean.to_string());
    }
    if clean.starts_with('/') || matches!(clean, "auto" | "local" | "daemon") {
        return Ok(clean.to_string());
    }
    Err(invalid())
}

impl Config {
    /// Pure resolver: no secret store, file access, or implicit environment reads.
    /// # Errors
    /// Missing forced destination or malformed enabled store URL, safely classified.
    pub fn resolve(
        choice: Option<bool>,
        url: Option<OsString>,
        netrc: Option<OsString>,
    ) -> io::Result<Self> {
        if choice == Some(false) {
            return Ok(Self::default());
        }
        let url = url.filter(|s| !s.is_empty());
        let Some(url) = url else {
            return if choice == Some(true) {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--cache-push requires MANDALA_CACHE_URL",
                ))
            } else {
                Ok(Self::default())
            };
        };
        let safe_url = Some(safe_url(url.to_str().ok_or_else(invalid)?)?);
        Ok(Self {
            url: Some(url),
            netrc: netrc.filter(|s| !s.is_empty()),
            safe_url,
        })
    }

    #[must_use]
    pub fn initial(&self) -> Value {
        self.record(
            if self.url.is_some() {
                "pending"
            } else {
                "skipped"
            },
            0,
            if self.url.is_none() {
                Some("disabled")
            } else {
                None
            },
            None,
        )
    }

    fn record(&self, outcome: &str, count: usize, reason: Option<&str>, rc: Option<i32>) -> Value {
        let mut value =
            json!({"enabled": self.url.is_some(), "outcome": outcome, "path_count": count});
        if let Some(url) = &self.safe_url {
            value["cache_url"] = json!(url);
        }
        if let Some(reason) = reason {
            value["reason"] = json!(reason);
        }
        if let Some(rc) = rc {
            value["process_rc"] = json!(rc);
        }
        value
    }

    fn argv(&self, paths: &BTreeSet<PathBuf>) -> Vec<OsString> {
        let mut args = vec![
            "copy".into(),
            "--to".into(),
            self.url.clone().expect("enabled"),
        ];
        if let Some(netrc) = &self.netrc {
            args.extend(["--option".into(), "netrc-file".into(), netrc.clone()]);
        }
        args.extend(paths.iter().map(|p| p.as_os_str().to_owned()));
        args
    }
}

fn persist(run: &RegisteredDeployRun, record: &Value) -> io::Result<()> {
    registry::update_meta(
        &run.path,
        Meta::from_iter([("publish".into(), record.clone())]),
    )
}

fn emit(writer: &EventWriter, record: &Value) -> io::Result<()> {
    let mut fields = record.as_object().expect("record object").clone();
    fields.insert("milestone".into(), json!("publish"));
    fields.insert("actor".into(), json!("controller"));
    writer.emit("milestone", fields)
}

/// Record a build-aborted skip without attempting upload effects.
/// # Errors
/// Registry or event I/O failures.
pub fn build_aborted(run: &RegisteredDeployRun) -> io::Result<()> {
    let config = &run.plan.publish;
    let record = config.record(
        "skipped",
        0,
        Some(if config.url.is_some() {
            "build-not-completed"
        } else {
            "disabled"
        }),
        None,
    );
    persist(run, &record)?;
    emit(
        &EventWriter::new(&run.path, "publish", "", "publish")?,
        &record,
    )
}

/// Publish once before host fan-out. Only durable I/O and interruption errors
/// propagate; ordinary upload failures are advisory. Raw backend output is
/// drained concurrently to a sink, never retained or forwarded.
/// # Errors
/// Registry/event I/O or explicit termination (never fall back after termination).
pub fn publish(
    run: &RegisteredDeployRun,
    built: &BuiltProfiles,
    program: &OsStr,
) -> io::Result<()> {
    let config = &run.plan.publish;
    let paths: BTreeSet<_> = built.paths.values().cloned().collect();
    let writer = EventWriter::new(&run.path, "publish", "", "publish")?;
    let record = if config.url.is_none() || paths.is_empty() {
        config.record(
            "skipped",
            paths.len(),
            Some(if config.url.is_none() {
                "disabled"
            } else {
                "no-built-paths"
            }),
            None,
        )
    } else {
        let running = config.record("running", paths.len(), None, None);
        persist(run, &running)?;
        emit(&writer, &running)?;
        eprintln!(
            "mandala: cache-publish: {} ({} paths)",
            config.safe_url.as_deref().unwrap_or("<redacted>"),
            paths.len()
        );
        let mut command = Command::new(program);
        command.args(config.argv(&paths));
        let result = mandala_deploy::process::discard_output_std(&mut command);
        if mandala_deploy::process::is_stopping()
            || result
                .as_ref()
                .is_err_and(|e| e.kind() == io::ErrorKind::Interrupted)
        {
            let interrupted = config.record("interrupted", paths.len(), Some("interrupted"), None);
            persist(run, &interrupted)?;
            emit(&writer, &interrupted)?;
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "cache publication interrupted",
            ));
        }
        match result {
            Ok(status) if status.success() => {
                config.record("succeeded", paths.len(), None, status.code())
            }
            Ok(status) => config.record("failed", paths.len(), Some("copy-exit"), status.code()),
            Err(_) => config.record("failed", paths.len(), Some("spawn-failed"), None),
        }
    };
    persist(run, &record)?;
    emit(&writer, &record)?;
    let summary = describe(&record);
    eprintln!("mandala: {summary}");
    if record["outcome"] == "failed" {
        writer.emit(
            "line",
            serde_json::Map::from_iter([("line".into(), json!(summary))]),
        )?;
    }
    // Attribute the shared operation to eligible hosts, without a terminal
    // status/rc and without inventing a controller fleet member.
    if config.url.is_some() {
        for host in built.paths.keys() {
            emit(&EventWriter::new(&run.path, host, host, "deploy")?, &record)?;
        }
    }
    Ok(())
}

/// Shared safe text used by headless, owned and attached observers.
#[must_use]
pub fn describe(record: &Value) -> String {
    format!(
        "cache-publish: {}{}{}{}{}",
        record["outcome"].as_str().unwrap_or("unknown"),
        record["cache_url"]
            .as_str()
            .map_or(String::new(), |s| format!(" {s}")),
        record["reason"]
            .as_str()
            .map_or(String::new(), |s| format!(" ({s})")),
        record["process_rc"]
            .as_i64()
            .map_or(String::new(), |s| format!(" rc={s}")),
        if record["outcome"] == "failed" {
            "; warning: continuing per-host SSH copy"
        } else {
            ""
        }
    )
}

#[cfg(test)]
mod tests {
    use super::super::deploy::DeployPlan;
    use super::*;
    use crate::runner::{EventTailer, HostState};
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    struct Fixture(RegisteredDeployRun);
    impl Fixture {
        fn new(config: Config) -> Self {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "mandala-publish-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            let run = RegisteredDeployRun {
                run_id: "fixture".into(),
                path,
                plan: DeployPlan {
                    limit: "a,b,c".into(),
                    targets: vec!["a".into(), "b".into(), "c".into()],
                    skipped: vec![],
                    settings: BTreeMap::new(),
                    dry_activate: false,
                    boot: false,
                    halt_on_build_failure: false,
                    publish: config,
                    throttle: 4,
                },
            };
            registry::write_meta(
                &run.path,
                &Meta::from_iter([("publish".into(), run.plan.publish.initial())]),
            )
            .unwrap();
            Self(run)
        }
        fn stub(&self, body: &str) -> PathBuf {
            let path = self.0.path.join("nix-stub");
            fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0.path);
        }
    }

    fn config() -> Config {
        Config::resolve(
            None,
            Some("https://user:password@cache.acme.example?token=hidden#secret".into()),
            Some("/fictional/private netrc".into()),
        )
        .unwrap()
    }
    fn built() -> BuiltProfiles {
        BuiltProfiles {
            paths: BTreeMap::from_iter([
                ("a".into(), "/nix/store/zzz-profile".into()),
                ("b".into(), "/nix/store/aaa-profile".into()),
                ("c".into(), "/nix/store/zzz-profile".into()),
            ]),
            failures: BTreeMap::new(),
        }
    }

    #[test]
    fn precedence_empty_netrc_only_and_safe_invalid_input() {
        for choice in [None, Some(false)] {
            assert_eq!(
                Config::resolve(choice, None, Some("netrc".into()))
                    .unwrap()
                    .initial()["outcome"],
                "skipped"
            );
            assert_eq!(
                Config::resolve(choice, Some("".into()), Some("".into()))
                    .unwrap()
                    .initial()["enabled"],
                false
            );
        }
        assert!(Config::resolve(Some(true), None, None).is_err());
        assert!(Config::resolve(Some(true), Some("".into()), None).is_err());
        assert!(
            Config::resolve(None, Some("https://cache.acme.example".into()), None)
                .unwrap()
                .netrc
                .is_none()
        );
        assert!(
            Config::resolve(
                Some(false),
                Some("invalid secret".into()),
                Some("netrc".into())
            )
            .is_ok()
        );
        let error = Config::resolve(None, Some("invalid secret".into()), None).unwrap_err();
        assert_eq!(error.to_string(), "invalid cache URL");
        assert_eq!(
            config().safe_url.as_deref(),
            Some("https://cache.acme.example")
        );
        assert!(!format!("{:?}", config()).contains("password"));
        for url in [
            "file:///tmp/fictional-cache",
            "s3://bucket?region=example",
            "ssh-ng://operator@builder.acme.example",
            "local",
            "/tmp/cache",
        ] {
            assert!(
                Config::resolve(None, Some(url.into()), None).is_ok(),
                "{url}"
            );
        }
    }

    #[test]
    fn flags_are_tristate_and_conflicts_fail_in_both_orders() {
        let command = || clap::Command::new("fixture").args(flags());
        for (args, expected) in [
            (vec!["fixture"], None),
            (vec!["fixture", "--cache-push"], Some(true)),
            (vec!["fixture", "--no-cache-push"], Some(false)),
        ] {
            assert_eq!(
                choice(&command().try_get_matches_from(args).unwrap()),
                expected
            );
        }
        for args in [
            ["fixture", "--cache-push", "--no-cache-push"],
            ["fixture", "--no-cache-push", "--cache-push"],
        ] {
            assert!(command().try_get_matches_from(args).is_err());
        }
    }

    #[test]
    fn union_upload_argv_is_exact_and_raw_output_is_discarded() {
        let fixture = Fixture::new(config());
        let trace = fixture.0.path.join("args");
        let stub = fixture.stub(&format!("printf '%s\\n' \"$@\" > '{}'\nprintf 'password hidden /fictional/private netrc'\nprintf 'secret' >&2\nexit 0", trace.display()));
        publish(&fixture.0, &built(), stub.as_os_str()).unwrap();
        assert_eq!(
            fs::read_to_string(trace).unwrap(),
            "copy\n--to\nhttps://user:password@cache.acme.example?token=hidden#secret\n--option\nnetrc-file\n/fictional/private netrc\n/nix/store/aaa-profile\n/nix/store/zzz-profile\n"
        );
        let meta = registry::read_meta(&fixture.0.path);
        assert_eq!(
            meta["publish"],
            json!({"enabled":true,"cache_url":"https://cache.acme.example","outcome":"succeeded","path_count":2,"process_rc":0})
        );
        assert!(meta.get("rc").is_none());
        let mut tailer = EventTailer::new(&fixture.0.path);
        tailer.poll();
        assert_eq!(tailer.publish.as_ref().unwrap()["outcome"], "succeeded");
        assert_eq!(tailer.hosts.len(), 3);
        for host in tailer.hosts.values() {
            assert_eq!(host.milestones, ["publish"]);
            assert_eq!(host.state, HostState::Pending);
            assert_eq!(host.rc, None);
        }
        for name in [
            "meta.json",
            "publish.jsonl",
            "a.jsonl",
            "b.jsonl",
            "c.jsonl",
        ] {
            let text = fs::read_to_string(fixture.0.path.join(name)).unwrap();
            for forbidden in ["password", "hidden", "/fictional/private netrc", "secret"] {
                assert!(!text.contains(forbidden), "{name}: {forbidden}");
            }
        }
    }

    #[test]
    fn failed_upload_and_spawn_are_advisory_and_do_not_fail_hosts() {
        for spawn in [false, true] {
            let fixture = Fixture::new(config());
            let stub = if spawn {
                fixture.0.path.join("missing-netrc-secret-program")
            } else {
                fixture.stub("echo credential >&2; exit 23")
            };
            publish(&fixture.0, &built(), stub.as_os_str()).unwrap();
            let meta = registry::read_meta(&fixture.0.path);
            assert_eq!(meta["publish"]["outcome"], "failed");
            assert_eq!(
                meta["publish"]["reason"],
                if spawn { "spawn-failed" } else { "copy-exit" }
            );
            assert!(meta.get("rc").is_none());
            let mut tailer = EventTailer::new(&fixture.0.path);
            tailer.poll();
            assert!(!tailer.has_unsuccessful_host());
            assert!(
                !fs::read_to_string(fixture.0.path.join("publish.jsonl"))
                    .unwrap()
                    .contains("credential")
            );
        }
    }

    #[test]
    fn disabled_empty_and_aborted_build_never_spawn() {
        for enabled in [false, true] {
            let fixture = Fixture::new(if enabled { config() } else { Config::default() });
            let empty = BuiltProfiles {
                paths: BTreeMap::new(),
                failures: BTreeMap::from_iter([("a".into(), "build failed".into())]),
            };
            let success = built();
            publish(
                &fixture.0,
                if enabled { &empty } else { &success },
                OsStr::new("must-not-exist"),
            )
            .unwrap();
            assert_eq!(
                registry::read_meta(&fixture.0.path)["publish"]["reason"],
                if enabled {
                    "no-built-paths"
                } else {
                    "disabled"
                }
            );
            build_aborted(&fixture.0).unwrap();
            assert_eq!(
                registry::read_meta(&fixture.0.path)["publish"]["reason"],
                if enabled {
                    "build-not-completed"
                } else {
                    "disabled"
                }
            );
        }
    }

    #[test]
    fn publish_record_shape_is_golden_and_legacy_records_have_no_phase() {
        let value = config().record("failed", 2, Some("copy-exit"), Some(23));
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            r#"{"cache_url":"https://cache.acme.example","enabled":true,"outcome":"failed","path_count":2,"process_rc":23,"reason":"copy-exit"}"#
        );
        let fixture = Fixture::new(Config::default());
        registry::write_meta(&fixture.0.path, &Meta::new()).unwrap();
        assert!(EventTailer::new(&fixture.0.path).publish.is_none());
    }
}
