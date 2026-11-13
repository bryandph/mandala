// SPDX-FileCopyrightText: 2020 Serokell <https://serokell.io/>
//
// SPDX-License-Identifier: MPL-2.0
//
// Vendored from serokell/deploy-rs@6d3087eedff75a715b40c0e124ba15d2dd7bec28.
// Mandala patch: accept a prebuilt profile, use caller-selected process
// programs for effect-isolated tests, and emit through DeployData::sink.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::process::Stdio;

use thiserror::Error;
use tokio::process::Command;

use crate::event::Level;
use crate::{DeployData, DeployDefs};

/// Additive per-host copy observation. Byte counts are uncompressed NAR
/// payload read by the controller, excluding SSH overhead and remote cache downloads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CopyStats {
    pub substitute_on_destination: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paths_copied: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_nar_bytes: Option<u64>,
}

impl CopyStats {
    fn new(substitute_on_destination: bool) -> Self {
        Self {
            substitute_on_destination,
            duration_ms: None,
            success: None,
            paths_copied: None,
            controller_nar_bytes: None,
        }
    }

    /// Human-readable projection shared by logs and observers.
    #[must_use]
    pub fn describe(&self) -> String {
        let number = |n: Option<u64>| n.map_or_else(|| "unknown".into(), |n| n.to_string());
        format!(
            "copy: substitute-on-destination={} paths={} controller-nar-bytes={} duration-ms={}",
            self.substitute_on_destination,
            number(self.paths_copied),
            number(self.controller_nar_bytes),
            number(self.duration_ms),
        )
    }

    fn read_stderr(&mut self, stderr: &str, destination: &str, sink: &dyn crate::EventSink) {
        // Nix logging.hh: actCopyPath=100, actCopyPaths=103, resProgress=105.
        // Progress is cumulative per activity, not an increment. Never count
        // file transfers/substitutions, expected sizes, or nested remote copies.
        let mut activities = BTreeMap::new();
        let mut progress = BTreeMap::new();
        let mut invalid = false;
        for line in stderr.lines() {
            let Some(json) = line.strip_prefix("@nix ") else {
                sink.emit(Level::Warn, line);
                continue;
            };
            let Ok(record) = serde_json::from_str::<Value>(json) else {
                invalid = true;
                continue;
            };
            match record["action"].as_str() {
                Some("msg") => {
                    if let Some(message) = record["msg"].as_str() {
                        sink.emit(Level::Warn, message);
                    } else {
                        invalid = true;
                    }
                }
                Some("start") => {
                    if let (Some(id), Some(kind)) = (record["id"].as_u64(), record["type"].as_u64())
                    {
                        // Remote substitution can emit its own copy-path
                        // activities. Only the requested controller destination
                        // counts toward our payload; aggregate progress is local.
                        let relevant = match kind {
                            100 => record["fields"][2].as_str() == Some(destination),
                            // Realisation also creates an empty copy-progress
                            // placeholder (type 103) without counters. The actual
                            // copyPaths activity has a nonempty description.
                            103 => {
                                record["parent"].as_u64() == Some(0)
                                    && record["text"].as_str().is_some_and(|text| !text.is_empty())
                            }
                            _ => false,
                        };
                        if relevant {
                            activities.insert(id, kind);
                        }
                        if matches!(kind, 100 | 103)
                            && let Some(text) = record["text"].as_str()
                            && !text.is_empty()
                        {
                            sink.emit(Level::Info, text);
                        }
                    } else {
                        invalid = true;
                    }
                }
                Some("result") if record["type"].as_u64() == Some(105) => {
                    if let Some(id) = record["id"].as_u64() {
                        if matches!(activities.get(&id), Some(100 | 103)) {
                            if let Some(done) = record["fields"][0].as_u64() {
                                progress.insert(id, done);
                            } else {
                                invalid = true;
                            }
                        }
                    } else {
                        invalid = true;
                    }
                }
                Some("result" | "stop") => {}
                _ => invalid = true,
            }
        }
        if invalid {
            sink.emit(
                Level::Warn,
                "copy telemetry unavailable: unrecognized Nix internal-json output",
            );
            return;
        }
        let sum = |kind| {
            let ids: Vec<_> = activities
                .iter()
                .filter(|(_, activity)| **activity == kind)
                .map(|(id, _)| id)
                .collect();
            if ids.is_empty() {
                None
            } else {
                ids.into_iter()
                    .try_fold(0_u64, |total, id| total.checked_add(*progress.get(id)?))
            }
        };
        self.paths_copied = sum(103);
        self.controller_nar_bytes = sum(100);
        if self.paths_copied.is_none() && self.controller_nar_bytes.is_none() {
            sink.emit(
                Level::Warn,
                "copy telemetry unavailable: Nix reported no recognized copy progress",
            );
        }
    }
}

#[derive(Error, Debug)]
pub enum PushProfileError {
    #[error("remoteBuild is outside the native-deploy contract")]
    RemoteBuildUnsupported,
    #[error("failed to run nix copy: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("nix copy failed with {0}")]
    Exit(std::process::ExitStatus),
}

/// Push an already-built activation closure to one host. Unlike upstream's
/// top-half path, this function cannot evaluate or build a flake.
pub async fn push_profile(
    deploy_data: &DeployData<'_>,
    deploy_defs: &DeployDefs,
    check_sigs: bool,
) -> Result<(), PushProfileError> {
    if deploy_data.merged_settings.remote_build == Some(true) {
        return Err(PushProfileError::RemoteBuildUnsupported);
    }

    deploy_data.sink.emit(
        Level::Info,
        &format!(
            "copy-start profile={} host={}",
            deploy_data.profile_name, deploy_data.node_name
        ),
    );
    let hostname = deploy_data
        .cmd_overrides
        .hostname
        .as_ref()
        .unwrap_or(&deploy_data.node.node_settings.hostname);
    let mut command = Command::new(
        deploy_data
            .cmd_overrides
            .nix_program
            .as_deref()
            .unwrap_or_else(|| Path::new("nix")),
    );
    command.args(["copy", "--log-format", "internal-json"]);
    if deploy_data.merged_settings.fast_connection != Some(true) {
        command.arg("--substitute-on-destination");
    }
    if !check_sigs {
        command.arg("--no-check-sigs");
    }
    let destination = format!("ssh://{}@{hostname}", deploy_defs.ssh_user);
    command
        .arg("--to")
        .arg(&destination)
        .arg(&deploy_data.profile.profile_settings.path)
        .env("NIX_SSHOPTS", deploy_data.nix_ssh_opts())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &deploy_data.cmd_overrides.environment {
        command.env(key, value);
    }

    let mut stats = CopyStats::new(deploy_data.merged_settings.fast_connection != Some(true));
    deploy_data.sink.copy(&stats);
    let started = Instant::now();
    let output = crate::process::output(&mut command).await;
    stats.duration_ms = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
    stats.success = Some(output.as_ref().is_ok_and(|output| output.status.success()));
    if let Ok(output) = &output {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            deploy_data.sink.emit(Level::Info, line);
        }
        stats.read_stderr(
            &String::from_utf8_lossy(&output.stderr),
            &destination,
            deploy_data.sink,
        );
    }
    deploy_data.sink.copy(&stats);
    deploy_data.sink.emit(Level::Info, &stats.describe());
    let output = output?;
    if !output.status.success() {
        return Err(PushProfileError::Exit(output.status));
    }
    deploy_data.sink.emit(Level::Info, "copy-complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Sink(Mutex<Vec<String>>);
    impl crate::EventSink for Sink {
        fn emit(&self, _: Level, message: &str) {
            self.0.lock().unwrap().push(message.to_string());
        }
    }

    #[test]
    fn recorded_nix_2_34_progress_ignores_realisation_placeholder() {
        let mut stats = CopyStats::new(false);
        stats.read_stderr(
            include_str!("fixtures/copy-nix-2.34.jsonl"),
            "local://",
            &Sink::default(),
        );
        assert_eq!(stats.paths_copied, Some(1));
        assert_eq!(stats.controller_nar_bytes, Some(4488));
    }

    #[test]
    fn nothing_to_copy_without_progress_does_not_invent_zero() {
        let mut stats = CopyStats::new(true);
        stats.read_stderr(concat!(
            "@nix {\"action\":\"start\",\"id\":1,\"level\":3,\"parent\":0,\"text\":\"copying 0 paths\",\"type\":103}\n",
            "@nix {\"action\":\"result\",\"fields\":[100,0],\"id\":1,\"type\":106}\n",
            "@nix {\"action\":\"stop\",\"id\":1}\n"
        ), "local://", &Sink::default());
        assert_eq!(stats.paths_copied, None);
        assert_eq!(stats.controller_nar_bytes, None);
    }

    #[test]
    fn cumulative_nar_progress_excludes_remote_substitution_and_other_transfers() {
        let sink = Sink::default();
        let mut stats = CopyStats::new(true);
        stats.read_stderr(concat!(
            "@nix {\"action\":\"start\",\"id\":1,\"type\":100,\"fields\":[\"path\",\"local\",\"ssh://deployer@host\"]}\n",
            "@nix {\"action\":\"result\",\"id\":1,\"type\":105,\"fields\":[10,30]}\n",
            "@nix {\"action\":\"result\",\"id\":1,\"type\":105,\"fields\":[30,30]}\n",
            "@nix {\"action\":\"start\",\"id\":2,\"type\":100,\"fields\":[\"path\",\"https://cache.example\",\"local\"]}\n",
            "@nix {\"action\":\"result\",\"id\":2,\"type\":105,\"fields\":[500,500]}\n",
            "@nix {\"action\":\"start\",\"id\":3,\"type\":101}\n",
            "@nix {\"action\":\"result\",\"id\":3,\"type\":105,\"fields\":[900,900]}\n"
        ), "ssh://deployer@host", &sink);
        assert_eq!(stats.controller_nar_bytes, Some(30));
        assert_eq!(stats.paths_copied, None);
    }

    #[test]
    fn malformed_internal_json_omits_counts_but_preserves_human_errors() {
        let sink = Sink::default();
        let mut stats = CopyStats::new(false);
        stats.read_stderr(concat!(
            "@nix {\"action\":\"start\",\"id\":1,\"type\":103,\"parent\":0,\"text\":\"copying paths\"}\n",
            "@nix {\"action\":\"result\",\"id\":1,\"type\":105,\"fields\":[2,2]}\n",
            "@nix broken\n",
            "@nix {\"action\":\"msg\",\"msg\":\"error: original failure\"}\n",
            "legacy stderr\n"
        ), "ssh://deployer@host", &sink);
        assert_eq!(stats.paths_copied, None);
        assert_eq!(stats.controller_nar_bytes, None);
        let lines = sink.0.lock().unwrap();
        assert!(lines.iter().any(|line| line == "copying paths"));
        assert!(lines.iter().any(|line| line == "error: original failure"));
        assert!(lines.iter().any(|line| line == "legacy stderr"));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("telemetry unavailable"))
        );
        assert!(!lines.iter().any(|line| line.starts_with("@nix")));
    }
}
