// SPDX-FileCopyrightText: 2020 Serokell <https://serokell.io/>
// SPDX-FileCopyrightText: 2020 Andreas Fuchs <asf@boinkor.net>
// SPDX-FileCopyrightText: 2021 Yannik Sander <contact@ysndr.de>
//
// SPDX-License-Identifier: MPL-2.0
//
// Vendored from serokell/deploy-rs@6d3087eedff75a715b40c0e124ba15d2dd7bec28.
// Spike patch: the dry-activation branch captures all output into the injected
// per-host sink. The magic-rollback branch lands in Stage B after this gate.

use std::path::Path;
use std::process::Stdio;

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

#[derive(Error, Debug)]
pub enum DeployProfileError {
    #[error("the spike gate only permits dry activation")]
    SpikeOnlyDryActivate,
    #[error("interactive sudo is not supported by the headless native engine")]
    InteractiveSudoUnsupported,
    #[error("deployment data invalid: {0}")]
    InvalidDeployData(#[from] DeployDataDefsError),
    #[error("failed to run activation over ssh: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("ssh activation failed with {0}")]
    Exit(std::process::ExitStatus),
}

/// Exercise deploy-rs's prebuilt-profile activation protocol without any
/// evaluator. The spike deliberately permits only `--dry-activate`; Stage B
/// promotes the fully vendored magic-rollback branch after this cut is proven.
pub async fn deploy_profile(
    deploy_data: &DeployData<'_>,
    deploy_defs: &DeployDefs,
    dry_activate: bool,
    boot: bool,
) -> Result<(), DeployProfileError> {
    if !dry_activate {
        return Err(DeployProfileError::SpikeOnlyDryActivate);
    }
    if deploy_data.merged_settings.interactive_sudo == Some(true) {
        return Err(DeployProfileError::InteractiveSudoUnsupported);
    }

    let temp_path = deploy_data
        .merged_settings
        .temp_path
        .as_deref()
        .unwrap_or_else(|| Path::new("/tmp"));
    let activate = build_activate_command(&ActivateCommandData {
        sudo: &deploy_defs.sudo,
        profile_info: &deploy_data.profile_info()?,
        closure: &deploy_data.profile.profile_settings.path,
        auto_rollback: deploy_data.merged_settings.auto_rollback.unwrap_or(true),
        temp_path,
        confirm_timeout: deploy_data.merged_settings.confirm_timeout.unwrap_or(30),
        magic_rollback: deploy_data.merged_settings.magic_rollback.unwrap_or(true),
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
            "activate-start profile={} host={} mode=dry",
            deploy_data.profile_name, deploy_data.node_name
        ),
    );

    let mut command = Command::new("ssh");
    command.arg(ssh_addr);
    command.args(&deploy_data.merged_settings.ssh_opts);
    command
        .arg(activate)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = command.output().await?;
    emit_output(deploy_data.sink, &output);
    if !output.status.success() {
        return Err(DeployProfileError::Exit(output.status));
    }
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
}
