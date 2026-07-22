// SPDX-FileCopyrightText: 2020 Serokell <https://serokell.io/>
// SPDX-FileCopyrightText: 2020 Andreas Fuchs <asf@boinkor.net>
// SPDX-FileCopyrightText: 2021 Yannik Sander <contact@ysndr.de>
//
// SPDX-License-Identifier: MPL-2.0
//
// Vendored from serokell/deploy-rs@6d3087eedff75a715b40c0e124ba15d2dd7bec28.
// Mandala patches: all subprocess output and controller diagnostics flow
// through DeployData::sink; caller-selected programs permit effect-isolated
// tests; stale canaries are cleared before a magic-rollback attempt; and
// post-wait activation/confirmation completion order preserves rollback
// semantics without parsing activate-rs output.

use std::path::Path;
use std::process::{ExitStatus, Stdio};

use thiserror::Error;
use tokio::process::Command;

use crate::event::{Level, emit_output};
use crate::{DeployData, DeployDataDefsError, DeployDefs, ProfileInfo};

struct ActivateCommandData<'a> {
    sudo: &'a Option<String>,
    profile_info: &'a ProfileInfo,
    closure: &'a str,
    auto_rollback: bool,
    temp_path: &'a Path,
    confirm_timeout: u16,
    magic_rollback: bool,
    dry_activate: bool,
    boot: bool,
}

fn build_activate_command(data: &ActivateCommandData<'_>) -> String {
    let mut command = format!(
        "{}/activate-rs activate '{}' {} --temp-path '{}' --confirm-timeout {}",
        data.closure,
        data.closure,
        match data.profile_info {
            ProfileInfo::ProfilePath { profile_path } => {
                format!("--profile-path '{profile_path}'")
            }
            ProfileInfo::ProfileUserAndName {
                profile_user,
                profile_name,
            } => format!("--profile-user {profile_user} --profile-name {profile_name}"),
        },
        data.temp_path.display(),
        data.confirm_timeout
    );
    if data.magic_rollback {
        command.push_str(" --magic-rollback");
    }
    if data.auto_rollback {
        command.push_str(" --auto-rollback");
    }
    if data.dry_activate {
        command.push_str(" --dry-activate");
    }
    if data.boot {
        command.push_str(" --boot");
    }
    if let Some(sudo) = data.sudo {
        command = format!("{sudo} {command}");
    }
    command
}

struct WaitCommandData<'a> {
    sudo: &'a Option<String>,
    closure: &'a str,
    temp_path: &'a Path,
    activation_timeout: Option<u16>,
}

fn build_wait_command(data: &WaitCommandData<'_>) -> String {
    let mut command = format!(
        "{}/activate-rs wait '{}' --temp-path '{}'",
        data.closure,
        data.closure,
        data.temp_path.display()
    );
    if let Some(timeout) = data.activation_timeout {
        command.push_str(&format!(" --activation-timeout {timeout}"));
    }
    if let Some(sudo) = data.sudo {
        command = format!("{sudo} {command}");
    }
    command
}

#[derive(Error, Debug)]
pub enum DeployProfileError {
    #[error("interactive sudo is not supported by the headless native engine")]
    InteractiveSudoUnsupported,
    #[error("deployment data invalid: {0}")]
    InvalidDeployData(#[from] DeployDataDefsError),
    #[error("failed to run activation over ssh: {0}")]
    ActivateSpawn(std::io::Error),
    #[error("ssh activation failed with {0}")]
    ActivateExit(ExitStatus),
    #[error("failed to clear stale deployment canary over ssh: {0}")]
    CanaryCleanupSpawn(std::io::Error),
    #[error("ssh stale deployment canary cleanup failed with {0}")]
    CanaryCleanupExit(ExitStatus),
    #[error("activation rolled back with {0} before confirmation completed")]
    RollbackExit(ExitStatus),
    #[error("failed to run activation waiter over ssh: {0}")]
    WaitSpawn(std::io::Error),
    #[error("ssh activation waiter failed with {0}")]
    WaitExit(ExitStatus),
    #[error("failed to run deployment confirmation over ssh: {0}")]
    ConfirmSpawn(std::io::Error),
    #[error("ssh deployment confirmation failed with {0}")]
    ConfirmExit(ExitStatus),
}

impl DeployProfileError {
    /// Once the waiter has completed, either a failed confirmation or the
    /// activation process winning with a non-zero exit means the canary was
    /// not removed in time and activate-rs restored the prior generation.
    #[must_use]
    pub fn rolled_back(&self) -> bool {
        matches!(
            self,
            Self::RollbackExit(_) | Self::ConfirmSpawn(_) | Self::ConfirmExit(_)
        )
    }
}

fn ssh_command(deploy_data: &DeployData<'_>, ssh_addr: &str) -> Command {
    let mut command = Command::new(
        deploy_data
            .cmd_overrides
            .ssh_program
            .as_deref()
            .unwrap_or_else(|| Path::new("ssh")),
    );
    command.arg(ssh_addr);
    command.args(deploy_data.ssh_args());
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &deploy_data.cmd_overrides.environment {
        command.env(key, value);
    }
    command
}

fn checked_output(
    output: std::process::Output,
    deploy_data: &DeployData<'_>,
    exit: impl FnOnce(ExitStatus) -> DeployProfileError,
) -> Result<(), DeployProfileError> {
    emit_output(deploy_data.sink, &output);
    if output.status.success() {
        Ok(())
    } else {
        Err(exit(output.status))
    }
}

async fn confirm_profile(
    deploy_data: &DeployData<'_>,
    deploy_defs: &DeployDefs,
    temp_path: &Path,
    ssh_addr: &str,
) -> Result<(), DeployProfileError> {
    let lock_path = crate::make_lock_path(temp_path, &deploy_data.profile.profile_settings.path);
    let mut confirm = format!("rm {}", lock_path.display());
    if let Some(sudo) = &deploy_defs.sudo {
        confirm = format!("{sudo} {confirm}");
    }
    deploy_data
        .sink
        .emit(Level::Debug, &format!("confirm-command {confirm}"));
    let mut command = ssh_command(deploy_data, ssh_addr);
    command.kill_on_drop(true);
    let output = command
        .arg(confirm)
        .output()
        .await
        .map_err(DeployProfileError::ConfirmSpawn)?;
    checked_output(output, deploy_data, DeployProfileError::ConfirmExit)?;
    deploy_data.sink.emit(Level::Info, "deployment confirmed");
    Ok(())
}

async fn clear_stale_canary(
    deploy_data: &DeployData<'_>,
    deploy_defs: &DeployDefs,
    temp_path: &Path,
    ssh_addr: &str,
) -> Result<(), DeployProfileError> {
    let lock_path = crate::make_lock_path(temp_path, &deploy_data.profile.profile_settings.path);
    let mut cleanup = format!("rm -f {}", lock_path.display());
    if let Some(sudo) = &deploy_defs.sudo {
        cleanup = format!("{sudo} {cleanup}");
    }
    deploy_data
        .sink
        .emit(Level::Debug, &format!("canary-cleanup-command {cleanup}"));
    let output = ssh_command(deploy_data, ssh_addr)
        .arg(cleanup)
        .output()
        .await
        .map_err(DeployProfileError::CanaryCleanupSpawn)?;
    checked_output(output, deploy_data, DeployProfileError::CanaryCleanupExit)?;
    deploy_data
        .sink
        .emit(Level::Info, "stale deployment canary cleared");
    Ok(())
}

/// Activate one already-built profile using deploy-rs's canary-lock protocol.
/// This is the recorded upstream activate/wait/confirm flow with every child
/// stream captured into the injected per-host sink.
pub async fn deploy_profile(
    deploy_data: &DeployData<'_>,
    deploy_defs: &DeployDefs,
    dry_activate: bool,
    boot: bool,
) -> Result<(), DeployProfileError> {
    if deploy_data.merged_settings.interactive_sudo == Some(true) {
        return Err(DeployProfileError::InteractiveSudoUnsupported);
    }

    let temp_path = deploy_data
        .merged_settings
        .temp_path
        .as_deref()
        .unwrap_or_else(|| Path::new("/tmp"));
    let magic_rollback = deploy_data.merged_settings.magic_rollback.unwrap_or(true);
    let activate = build_activate_command(&ActivateCommandData {
        sudo: &deploy_defs.sudo,
        profile_info: &deploy_data.profile_info()?,
        closure: &deploy_data.profile.profile_settings.path,
        auto_rollback: deploy_data.merged_settings.auto_rollback.unwrap_or(true),
        temp_path,
        confirm_timeout: deploy_data.merged_settings.confirm_timeout.unwrap_or(30),
        magic_rollback,
        dry_activate,
        boot,
    });
    let hostname = deploy_data
        .cmd_overrides
        .hostname
        .as_ref()
        .unwrap_or(&deploy_data.node.node_settings.hostname);
    let ssh_addr = format!("{}@{hostname}", deploy_defs.ssh_user);
    deploy_data.sink.emit(
        Level::Info,
        &format!(
            "activate-start profile={} host={} dry={} boot={}",
            deploy_data.profile_name, deploy_data.node_name, dry_activate, boot
        ),
    );
    deploy_data
        .sink
        .emit(Level::Debug, &format!("activate-command {activate}"));

    if !magic_rollback || dry_activate || boot {
        let output = ssh_command(deploy_data, &ssh_addr)
            .arg(activate)
            .output()
            .await
            .map_err(DeployProfileError::ActivateSpawn)?;
        checked_output(output, deploy_data, DeployProfileError::ActivateExit)?;
        deploy_data.sink.emit(Level::Info, "activate-complete");
        return Ok(());
    }

    let wait = build_wait_command(&WaitCommandData {
        sudo: &deploy_defs.sudo,
        closure: &deploy_data.profile.profile_settings.path,
        temp_path,
        activation_timeout: deploy_data.merged_settings.activation_timeout,
    });
    deploy_data
        .sink
        .emit(Level::Debug, &format!("wait-command {wait}"));

    // activate-rs leaves its deterministic canary behind when confirmation
    // times out. Its waiter treats mere existence as readiness, so a retry of
    // the same immutable closure must remove that stale predecessor before
    // either the new activation or waiter can observe it.
    clear_stale_canary(deploy_data, deploy_defs, temp_path, &ssh_addr).await?;

    let activate_child = ssh_command(deploy_data, &ssh_addr)
        .arg(activate)
        .spawn()
        .map_err(DeployProfileError::ActivateSpawn)?;
    let wait_child = ssh_command(deploy_data, &ssh_addr)
        .arg(wait)
        .spawn()
        .map_err(DeployProfileError::WaitSpawn)?;
    let mut activate_output = Box::pin(activate_child.wait_with_output());
    let mut wait_output = Box::pin(wait_child.wait_with_output());
    let mut activation_finished = None;

    tokio::select! {
        output = &mut wait_output => {
            let output = output.map_err(DeployProfileError::WaitSpawn)?;
            checked_output(output, deploy_data, DeployProfileError::WaitExit)?;
            deploy_data.sink.emit(Level::Info, "activation waiter complete");
        }
        output = &mut activate_output => {
            let output = output.map_err(DeployProfileError::ActivateSpawn)?;
            emit_output(deploy_data.sink, &output);
            if !output.status.success() {
                return Err(DeployProfileError::ActivateExit(output.status));
            }
            activation_finished = Some(output);
            deploy_data.sink.emit(Level::Debug, "activation finished before waiter");
        }
    }

    deploy_data
        .sink
        .emit(Level::Info, "attempting deployment confirmation");
    let mut confirmation = Box::pin(confirm_profile(
        deploy_data,
        deploy_defs,
        temp_path,
        &ssh_addr,
    ));
    let confirmation = if activation_finished.is_none() {
        tokio::select! {
            // If both children settle in one poll, the activation result is
            // the protocol authority: confirmation did not complete in time.
            biased;
            output = &mut activate_output => {
                let output = output.map_err(DeployProfileError::ActivateSpawn)?;
                emit_output(deploy_data.sink, &output);
                if !output.status.success() {
                    return Err(DeployProfileError::RollbackExit(output.status));
                }
                confirmation.await
            }
            result = &mut confirmation => {
                let output = activate_output
                    .await
                    .map_err(DeployProfileError::ActivateSpawn)?;
                emit_output(deploy_data.sink, &output);
                // A failed confirmation deliberately leaves the canary in
                // place, so its typed rollback result wins the expected
                // non-zero activate-rs exit. A timely successful confirmation
                // still requires activation itself to succeed.
                if result.is_ok() && !output.status.success() {
                    return Err(DeployProfileError::ActivateExit(output.status));
                }
                result
            }
        }
    } else {
        confirmation.await
    };
    if confirmation.is_err() {
        deploy_data.sink.emit(
            Level::Error,
            "deployment confirmation failed; activate-rs rolled back",
        );
    }
    confirmation?;
    deploy_data.sink.emit(Level::Info, "activate-complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_command_matches_upstream_dry_branch() {
        let sudo = None;
        let info = ProfileInfo::ProfileUserAndName {
            profile_user: "root".into(),
            profile_name: "system".into(),
        };
        assert_eq!(
            build_activate_command(&ActivateCommandData {
                sudo: &sudo,
                profile_info: &info,
                closure: "/nix/store/example-profile",
                auto_rollback: true,
                temp_path: Path::new("/tmp"),
                confirm_timeout: 30,
                magic_rollback: true,
                dry_activate: true,
                boot: false,
            }),
            "/nix/store/example-profile/activate-rs activate '/nix/store/example-profile' --profile-user root --profile-name system --temp-path '/tmp' --confirm-timeout 30 --magic-rollback --auto-rollback --dry-activate"
        );
    }

    #[test]
    fn wait_command_carries_activation_timeout() {
        let sudo = Some("sudo -u app".into());
        assert_eq!(
            build_wait_command(&WaitCommandData {
                sudo: &sudo,
                closure: "/nix/store/example-profile",
                temp_path: Path::new("/run/deploy"),
                activation_timeout: Some(90),
            }),
            "sudo -u app /nix/store/example-profile/activate-rs wait '/nix/store/example-profile' --temp-path '/run/deploy' --activation-timeout 90"
        );
    }
}
