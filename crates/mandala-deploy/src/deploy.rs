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

/// The activation path selected after the copied generation has had the
/// opportunity to apply its own switch-inhibitor policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationDisposition {
    /// A non-mutating deploy preview. `reboot_pending` describes what a real
    /// deployment would do.
    DryPreview { reboot_pending: bool },
    /// The generation is safe to activate through deploy-rs's existing
    /// switch/wait/confirm protocol.
    LiveSwitch,
    /// The generation must be installed as the next boot default without live
    /// activation.
    BootStaging,
}

impl ActivationDisposition {
    #[must_use]
    pub fn reboot_pending(self) -> bool {
        matches!(
            self,
            Self::BootStaging
                | Self::DryPreview {
                    reboot_pending: true
                }
        )
    }
}

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
    sanitized_remote_command(data.sudo, &command)
}

fn sanitized_remote_command(sudo: &Option<String>, command: &str) -> String {
    match sudo {
        Some(sudo) => {
            format!("env -u NIXOS_NO_CHECK {sudo} env -u NIXOS_NO_CHECK {command}")
        }
        None => format!("env -u NIXOS_NO_CHECK {command}"),
    }
}

fn profile_path(profile_info: &ProfileInfo) -> String {
    match profile_info {
        ProfileInfo::ProfilePath { profile_path } => profile_path.clone(),
        ProfileInfo::ProfileUserAndName {
            profile_user,
            profile_name,
        } if profile_user == "root" && profile_name == "system" => {
            "/nix/var/nix/profiles/system".to_string()
        }
        ProfileInfo::ProfileUserAndName {
            profile_user,
            profile_name,
        } => format!("/nix/var/nix/profiles/per-user/{profile_user}/{profile_name}"),
    }
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
    #[error("activation rolled back with {activate_status} after waiter failed with {wait_status}")]
    WaitRollback {
        wait_status: ExitStatus,
        activate_status: ExitStatus,
    },
    #[error(
        "activation did not finish within {timeout_secs}s after waiter failed with {wait_status}"
    )]
    WaitActivationTimeout {
        wait_status: ExitStatus,
        timeout_secs: u16,
    },
    #[error("failed to run activation waiter over ssh: {0}")]
    WaitSpawn(std::io::Error),
    #[error("ssh activation waiter failed with {0}")]
    WaitExit(ExitStatus),
    #[error("failed to run deployment confirmation over ssh: {0}")]
    ConfirmSpawn(std::io::Error),
    #[error("ssh deployment confirmation failed with {0}")]
    ConfirmExit(ExitStatus),
    #[error("failed to probe target switch-inhibitor support over ssh: {0}")]
    CapabilityProbeSpawn(std::io::Error),
    #[error("target switch-inhibitor capability probe failed with {0}")]
    CapabilityProbeExit(ExitStatus),
    #[error("failed to run target switch preflight over ssh: {0}")]
    PreflightSpawn(std::io::Error),
    #[error("target switch preflight transport failed with {0}")]
    PreflightTransport(ExitStatus),
    #[error("failed to resolve the previous system profile over ssh: {0}")]
    PreviousProfileSpawn(std::io::Error),
    #[error("previous system profile could not be resolved: {0}")]
    PreviousProfileExit(ExitStatus),
    #[error("previous system profile resolved to an empty or invalid path")]
    PreviousProfileInvalid,
    #[error("boot-only staging failed: {primary}; recovery: {recovery}")]
    BootStage { primary: String, recovery: String },
}

impl DeployProfileError {
    /// Typed rollback outcomes include failed confirmation/canary expiry and
    /// a failed waiter followed by completed nonzero activation when automatic
    /// rollback is enabled. A timed-out activation remains a failure: the
    /// controller cannot claim that remote recovery completed.
    #[must_use]
    pub fn rolled_back(&self) -> bool {
        matches!(
            self,
            Self::RollbackExit(_)
                | Self::WaitRollback { .. }
                | Self::ConfirmSpawn(_)
                | Self::ConfirmExit(_)
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

/// Start the opt-in VM owner helper through precisely the existing owner
/// transport and flattened privilege settings. The caller owns its process
/// group and bounded stdio protocol; no guest address is involved.
#[must_use]
pub fn owner_session_command(deploy_data: &DeployData<'_>, deploy_defs: &DeployDefs) -> Command {
    let address = remote_address(deploy_data, deploy_defs);
    let mut command = ssh_command(deploy_data, &address);
    let helper = match &deploy_defs.sudo {
        Some(sudo) => format!("{sudo} /run/current-system/sw/bin/mandala-vm-owner"),
        None => "/run/current-system/sw/bin/mandala-vm-owner".into(),
    };
    command.arg(helper).stdin(Stdio::piped());
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

fn remote_address(deploy_data: &DeployData<'_>, deploy_defs: &DeployDefs) -> String {
    let hostname = deploy_data
        .cmd_overrides
        .hostname
        .as_ref()
        .unwrap_or(&deploy_data.node.node_settings.hostname);
    format!("{}@{hostname}", deploy_defs.ssh_user)
}

/// Resolve the safe activation path after copy and before any profile or
/// runtime mutation. An explicit boot request bypasses live-switch preflight;
/// older closures without the capability marker retain legacy switch behavior.
pub async fn resolve_activation_disposition(
    deploy_data: &DeployData<'_>,
    deploy_defs: &DeployDefs,
    dry_activate: bool,
    boot: bool,
) -> Result<ActivationDisposition, DeployProfileError> {
    if boot {
        return Ok(if dry_activate {
            ActivationDisposition::DryPreview {
                reboot_pending: true,
            }
        } else {
            ActivationDisposition::BootStaging
        });
    }

    let closure = &deploy_data.profile.profile_settings.path;
    let ssh_addr = remote_address(deploy_data, deploy_defs);
    let capability = format!("test -e {}/switch-inhibitors", crate::shell_word(closure));
    deploy_data.sink.emit(
        Level::Debug,
        &format!("switch-capability-command {capability}"),
    );
    let output = crate::process::output(ssh_command(deploy_data, &ssh_addr).arg(capability))
        .await
        .map_err(DeployProfileError::CapabilityProbeSpawn)?;
    emit_output(deploy_data.sink, &output);
    match output.status.code() {
        Some(0) => {}
        Some(1) => {
            deploy_data.sink.emit(
                Level::Info,
                "target generation has no switch-inhibitor contract; preserving legacy switch behavior",
            );
            return Ok(if dry_activate {
                ActivationDisposition::DryPreview {
                    reboot_pending: false,
                }
            } else {
                ActivationDisposition::LiveSwitch
            });
        }
        _ => return Err(DeployProfileError::CapabilityProbeExit(output.status)),
    }

    let check = sanitized_remote_command(
        &deploy_defs.sudo,
        &format!(
            "{}/bin/switch-to-configuration check",
            crate::shell_word(closure)
        ),
    );
    deploy_data
        .sink
        .emit(Level::Debug, &format!("switch-preflight-command {check}"));
    let output = crate::process::output(ssh_command(deploy_data, &ssh_addr).arg(check))
        .await
        .map_err(DeployProfileError::PreflightSpawn)?;
    emit_output(deploy_data.sink, &output);
    let reboot_pending = if output.status.success() {
        deploy_data.sink.emit(
            Level::Info,
            "target switch preflight accepted live activation",
        );
        false
    } else if output.status.code() == Some(255) {
        return Err(DeployProfileError::PreflightTransport(output.status));
    } else {
        deploy_data.sink.emit(
            Level::Info,
            "target switch preflight refused live activation; selecting boot-only staging",
        );
        true
    };
    Ok(if dry_activate {
        ActivationDisposition::DryPreview { reboot_pending }
    } else if reboot_pending {
        ActivationDisposition::BootStaging
    } else {
        ActivationDisposition::LiveSwitch
    })
}

async fn boot_stage_command(
    deploy_data: &DeployData<'_>,
    ssh_addr: &str,
    label: &str,
    command: String,
) -> Result<(), String> {
    deploy_data
        .sink
        .emit(Level::Debug, &format!("{label}-command {command}"));
    let output = crate::process::output(ssh_command(deploy_data, ssh_addr).arg(command))
        .await
        .map_err(|error| format!("{label} spawn failed: {error}"))?;
    emit_output(deploy_data.sink, &output);
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("{label} exited with {}", output.status))
    }
}

async fn recover_boot_stage(
    deploy_data: &DeployData<'_>,
    deploy_defs: &DeployDefs,
    ssh_addr: &str,
    profile_path: &str,
    previous: &str,
) -> String {
    let restore_profile = sanitized_remote_command(
        &deploy_defs.sudo,
        &format!(
            "nix-env --profile {} --set {}",
            crate::shell_word(profile_path),
            crate::shell_word(previous)
        ),
    );
    let restore_boot = sanitized_remote_command(
        &deploy_defs.sudo,
        &format!(
            "{}/bin/switch-to-configuration boot",
            crate::shell_word(previous)
        ),
    );
    let mut errors = Vec::new();
    if let Err(error) = boot_stage_command(
        deploy_data,
        ssh_addr,
        "boot-recovery-profile",
        restore_profile,
    )
    .await
    {
        errors.push(error);
    }
    if let Err(error) =
        boot_stage_command(deploy_data, ssh_addr, "boot-recovery-default", restore_boot).await
    {
        errors.push(error);
    }
    if errors.is_empty() {
        "restored previous profile and boot default".to_string()
    } else {
        errors.join("; ")
    }
}

/// Install the copied generation as the next boot default without live
/// activation. Failure after mutation restores both the prior profile and its
/// boot default using boot-only operations.
pub async fn stage_profile_for_boot(
    deploy_data: &DeployData<'_>,
    deploy_defs: &DeployDefs,
) -> Result<(), DeployProfileError> {
    if deploy_data.merged_settings.interactive_sudo == Some(true) {
        return Err(DeployProfileError::InteractiveSudoUnsupported);
    }
    let info = deploy_data.profile_info()?;
    let profile_path = profile_path(&info);
    let closure = &deploy_data.profile.profile_settings.path;
    let ssh_addr = remote_address(deploy_data, deploy_defs);
    let resolve = sanitized_remote_command(
        &deploy_defs.sudo,
        &format!("readlink -e {}", crate::shell_word(&profile_path)),
    );
    deploy_data
        .sink
        .emit(Level::Debug, &format!("previous-profile-command {resolve}"));
    let output = crate::process::output(ssh_command(deploy_data, &ssh_addr).arg(resolve))
        .await
        .map_err(DeployProfileError::PreviousProfileSpawn)?;
    emit_output(deploy_data.sink, &output);
    if !output.status.success() {
        return Err(DeployProfileError::PreviousProfileExit(output.status));
    }
    let previous = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !previous.starts_with("/nix/store/") {
        return Err(DeployProfileError::PreviousProfileInvalid);
    }

    let set_profile = sanitized_remote_command(
        &deploy_defs.sudo,
        &format!(
            "nix-env --profile {} --set {}",
            crate::shell_word(&profile_path),
            crate::shell_word(closure)
        ),
    );
    if let Err(primary) =
        boot_stage_command(deploy_data, &ssh_addr, "boot-stage-profile", set_profile).await
    {
        let recovery = recover_boot_stage(
            deploy_data,
            deploy_defs,
            &ssh_addr,
            &profile_path,
            &previous,
        )
        .await;
        return Err(DeployProfileError::BootStage { primary, recovery });
    }

    let install_boot = sanitized_remote_command(
        &deploy_defs.sudo,
        &format!(
            "{}/bin/switch-to-configuration boot",
            crate::shell_word(closure)
        ),
    );
    if let Err(primary) =
        boot_stage_command(deploy_data, &ssh_addr, "boot-stage-default", install_boot).await
    {
        let recovery = recover_boot_stage(
            deploy_data,
            deploy_defs,
            &ssh_addr,
            &profile_path,
            &previous,
        )
        .await;
        return Err(DeployProfileError::BootStage { primary, recovery });
    }
    deploy_data.sink.emit(
        Level::Info,
        "boot-only staging complete; target requires reboot",
    );
    Ok(())
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
    let output = crate::process::output(ssh_command(deploy_data, ssh_addr).arg(confirm))
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
    // deploy-rs v1.0.0 derives both sentinels from the closure's store hash.
    let cancel_path =
        crate::make_cancel_path(temp_path, &deploy_data.profile.profile_settings.path);
    let mut cleanup = format!(
        "rm -f {} {}",
        crate::shell_word(&lock_path.to_string_lossy()),
        crate::shell_word(&cancel_path.to_string_lossy())
    );
    if let Some(sudo) = &deploy_defs.sudo {
        cleanup = format!("{sudo} {cleanup}");
    }
    deploy_data
        .sink
        .emit(Level::Debug, &format!("canary-cleanup-command {cleanup}"));
    let output = crate::process::output(ssh_command(deploy_data, ssh_addr).arg(cleanup))
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
        boot: false,
    });
    let ssh_addr = remote_address(deploy_data, deploy_defs);
    deploy_data.sink.emit(
        Level::Info,
        &format!(
            "activate-start profile={} host={} dry={}",
            deploy_data.profile_name, deploy_data.node_name, dry_activate
        ),
    );
    deploy_data
        .sink
        .emit(Level::Debug, &format!("activate-command {activate}"));

    if !magic_rollback || dry_activate {
        let output = crate::process::output(ssh_command(deploy_data, &ssh_addr).arg(activate))
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
    // either the new activation or waiter can observe it. v1.0.0 also leaves
    // a cancel sentinel on failure; wait checks it at startup, so clear both.
    clear_stale_canary(deploy_data, deploy_defs, temp_path, &ssh_addr).await?;

    let (activate_child, activate_group) =
        crate::process::spawn(ssh_command(deploy_data, &ssh_addr).arg(activate))
            .map_err(DeployProfileError::ActivateSpawn)?;
    // Construct the guarded future before spawning the waiter: a waiter-spawn
    // failure must also drop and terminate activation, not detach it.
    let mut activate_output = Box::pin(crate::process::wait_with_output(
        activate_child,
        activate_group,
    ));
    let (wait_child, wait_group) =
        crate::process::spawn(ssh_command(deploy_data, &ssh_addr).arg(wait))
            .map_err(DeployProfileError::WaitSpawn)?;
    let mut wait_output = Box::pin(crate::process::wait_with_output(wait_child, wait_group));
    let mut activation_finished = None;

    tokio::select! {
        // If both children settle in one poll, the waiter is the protocol
        // authority: activate-rs only creates the canary the waiter is
        // watching for once the new generation is live, so a non-zero
        // activation exit alongside a satisfied waiter is a rollback (handled
        // by the second select), not an activation failure.
        biased;
        output = &mut wait_output => {
            let output = output.map_err(DeployProfileError::WaitSpawn)?;
            emit_output(deploy_data.sink, &output);
            if !output.status.success() {
                // activate-rs 1.0 signals cancel BEFORE running deactivate().
                // Keep the owned activation future and its output pipes alive
                // until rollback finishes. Cancellation/termination still drops
                // the guard; a bounded timeout uses that same group-kill path.
                let timeout_secs = deploy_data.merged_settings.activation_timeout.unwrap_or(240);
                deploy_data.sink.emit(Level::Info, "activation waiter failed; waiting for activation/rollback to finish");
                let activation = tokio::time::timeout(
                    std::time::Duration::from_secs(u64::from(timeout_secs)),
                    &mut activate_output,
                )
                .await
                .map_err(|_| DeployProfileError::WaitActivationTimeout {
                    wait_status: output.status,
                    timeout_secs,
                })?
                .map_err(DeployProfileError::ActivateSpawn)?;
                emit_output(deploy_data.sink, &activation);
                return Err(if !activation.status.success()
                    && deploy_data.merged_settings.auto_rollback.unwrap_or(true)
                {
                    DeployProfileError::WaitRollback {
                        wait_status: output.status,
                        activate_status: activation.status,
                    }
                } else {
                    DeployProfileError::WaitExit(output.status)
                });
            }
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
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::*;
    use crate::data::{GenericSettings, Node, NodeSettings, Profile, ProfileSettings};

    #[test]
    fn vm_owner_backend_helper_uses_owner_transport_and_flattened_privilege() {
        let profile = Profile {
            profile_settings: ProfileSettings {
                path: "/nix/store/profile".into(),
                profile_path: None,
            },
            generic_settings: GenericSettings::default(),
        };
        let node = Node {
            generic_settings: GenericSettings {
                ssh_user: Some("operator-alice".into()),
                user: Some("root".into()),
                ssh_port: Some(2222),
                identity_file: Some("/tmp/owner-key".into()),
                ssh_opts: vec!["-oStrictHostKeyChecking=yes".into()],
                sudo: Some("sudo -u".into()),
                ..GenericSettings::default()
            },
            node_settings: NodeSettings {
                hostname: "owner.example.test".into(),
                profiles: HashMap::from([("system".into(), profile.clone())]),
                profiles_order: Vec::new(),
            },
        };
        let sink = RecordingSink::default();
        let overrides = crate::CmdOverrides::default();
        let data = crate::make_deploy_data(
            &GenericSettings::default(),
            &node,
            "host-a",
            &profile,
            "system",
            &overrides,
            &sink,
        );
        let defs = data.defs().unwrap();
        let command = owner_session_command(&data, &defs);
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[0], "operator-alice@owner.example.test");
        assert!(args.contains(&"2222".into()));
        assert!(args.contains(&"/tmp/owner-key".into()));
        assert!(args.contains(&"-oStrictHostKeyChecking=yes".into()));
        assert_eq!(
            args.last().unwrap(),
            "sudo -u root /run/current-system/sw/bin/mandala-vm-owner"
        );
        assert!(args.iter().all(|a| !a.contains("guest")));
    }

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<String>>);

    impl crate::EventSink for RecordingSink {
        fn emit(&self, _level: Level, message: &str) {
            self.0.lock().unwrap().push(message.to_string());
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "mandala-deploy-{name}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ))
    }

    fn effect_program(name: &str, body: &str) -> (PathBuf, PathBuf) {
        let base = temp_path(name);
        std::fs::create_dir_all(&base).unwrap();
        let trace = base.join("trace");
        let program = base.join("ssh");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n{}\n",
                trace.display(),
                body
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&program, permissions).unwrap();
        (program, trace)
    }

    async fn disposition_case(
        name: &str,
        body: &str,
        dry: bool,
        boot: bool,
    ) -> (
        Result<ActivationDisposition, DeployProfileError>,
        String,
        Vec<String>,
    ) {
        let (ssh, trace) = effect_program(name, body);
        let profile = Profile {
            profile_settings: ProfileSettings {
                path: "/nix/store/00000000000000000000000000000000-new-system".into(),
                profile_path: None,
            },
            generic_settings: GenericSettings::default(),
        };
        let node = Node {
            generic_settings: GenericSettings {
                ssh_user: Some("root".into()),
                user: Some("root".into()),
                sudo: Some("sudo -u".into()),
                ..GenericSettings::default()
            },
            node_settings: NodeSettings {
                hostname: "host.example.test".into(),
                profiles: HashMap::from([("system".into(), profile.clone())]),
                profiles_order: Vec::new(),
            },
        };
        let overrides = crate::CmdOverrides {
            ssh_program: Some(ssh),
            environment: vec![("NIXOS_NO_CHECK".into(), "1".into())],
            ..crate::CmdOverrides::default()
        };
        let sink = RecordingSink::default();
        let deploy = crate::make_deploy_data(
            &GenericSettings::default(),
            &node,
            "host",
            &profile,
            "system",
            &overrides,
            &sink,
        );
        let defs = deploy.defs().unwrap();
        let result = resolve_activation_disposition(&deploy, &defs, dry, boot).await;
        let trace = std::fs::read_to_string(trace).unwrap_or_default();
        let messages = sink.0.into_inner().unwrap();
        (result, trace, messages)
    }

    async fn boot_stage_case(
        name: &str,
        body: &str,
    ) -> (Result<(), DeployProfileError>, String, Vec<String>) {
        let (ssh, trace) = effect_program(name, body);
        let profile = Profile {
            profile_settings: ProfileSettings {
                path: "/nix/store/00000000000000000000000000000000-new-system".into(),
                profile_path: None,
            },
            generic_settings: GenericSettings::default(),
        };
        let node = Node {
            generic_settings: GenericSettings {
                ssh_user: Some("root".into()),
                user: Some("root".into()),
                sudo: Some("sudo -u".into()),
                ..GenericSettings::default()
            },
            node_settings: NodeSettings {
                hostname: "host.example.test".into(),
                profiles: HashMap::from([("system".into(), profile.clone())]),
                profiles_order: Vec::new(),
            },
        };
        let overrides = crate::CmdOverrides {
            ssh_program: Some(ssh),
            environment: vec![("NIXOS_NO_CHECK".into(), "1".into())],
            ..crate::CmdOverrides::default()
        };
        let sink = RecordingSink::default();
        let deploy = crate::make_deploy_data(
            &GenericSettings::default(),
            &node,
            "host",
            &profile,
            "system",
            &overrides,
            &sink,
        );
        let defs = deploy.defs().unwrap();
        let result = stage_profile_for_boot(&deploy, &defs).await;
        let trace = std::fs::read_to_string(trace).unwrap_or_default();
        let messages = sink.0.into_inner().unwrap();
        (result, trace, messages)
    }

    async fn cancel_case(
        name: &str,
        auto_rollback: bool,
        timeout: Option<u16>,
        body: &str,
    ) -> (Result<(), DeployProfileError>, Vec<String>, PathBuf) {
        let (ssh, _) = effect_program(name, body);
        let base = ssh.parent().unwrap().to_path_buf();
        let closure = "/nix/store/00000000000000000000000000000000-new-system";
        // Both sentinels really exist before deploy starts, not just in argv.
        for path in [
            crate::make_lock_path(&base, closure),
            crate::make_cancel_path(&base, closure),
        ] {
            std::fs::write(path, "stale").unwrap();
        }
        let profile = Profile {
            profile_settings: ProfileSettings {
                path: closure.into(),
                profile_path: None,
            },
            generic_settings: GenericSettings::default(),
        };
        let node = Node {
            generic_settings: GenericSettings {
                ssh_user: Some("root".into()),
                user: Some("root".into()),
                auto_rollback: Some(auto_rollback),
                activation_timeout: timeout,
                temp_path: Some(base.clone()),
                ..GenericSettings::default()
            },
            node_settings: NodeSettings {
                hostname: "host.invalid".into(),
                profiles: HashMap::from([("system".into(), profile.clone())]),
                profiles_order: Vec::new(),
            },
        };
        let overrides = crate::CmdOverrides {
            ssh_program: Some(ssh),
            environment: vec![("TEST_ROOT".into(), base.to_string_lossy().into_owned())],
            ..crate::CmdOverrides::default()
        };
        let sink = RecordingSink::default();
        let deploy = crate::make_deploy_data(
            &GenericSettings::default(),
            &node,
            "host",
            &profile,
            "system",
            &overrides,
            &sink,
        );
        let result = deploy_profile(&deploy, &deploy.defs().unwrap(), false).await;
        (result, sink.0.into_inner().unwrap(), base)
    }

    const CANCEL_EFFECT: &str = r#"for command do :; done
case "$command" in
  'rm -f '*) sh -c "$command"; exit $? ;;
  *'activate-rs activate '*)
    test ! -e "$TEST_ROOT/deploy-rs-canary-00000000000000000000000000000000" || exit 91
    test ! -e "$TEST_ROOT/deploy-rs-cancel-00000000000000000000000000000000" || exit 92
    touch "$TEST_ROOT/deploy-rs-cancel-00000000000000000000000000000000"
    touch "$TEST_ROOT/clean-start"
    while [ ! -f "$TEST_ROOT/waited" ]; do sleep 0.02; done
    sleep 0.1
    echo rollback-finished-stdout
    echo rollback-finished-stderr >&2
    touch "$TEST_ROOT/finished"
    exit 1 ;;
  *'activate-rs wait '*)
    while [ ! -f "$TEST_ROOT/clean-start" ]; do sleep 0.02; done
    test -e "$TEST_ROOT/deploy-rs-cancel-00000000000000000000000000000000" || exit 93
    touch "$TEST_ROOT/waited"
    exit 1 ;;
esac
exit 99"#;

    #[tokio::test]
    async fn cancel_waits_for_rollback_output_and_has_typed_result() {
        let (result, messages, base) =
            cancel_case("cancel-rollback", true, None, CANCEL_EFFECT).await;
        let error = result.unwrap_err();
        assert!(
            matches!(error, DeployProfileError::WaitRollback { .. }),
            "{error}"
        );
        assert!(error.rolled_back());
        assert!(base.join("finished").exists());
        assert!(
            messages
                .iter()
                .any(|m| m.contains("rollback-finished-stdout"))
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("rollback-finished-stderr"))
        );
    }

    #[tokio::test]
    async fn cancel_without_auto_rollback_is_failed_after_activation_finishes() {
        let (result, _, base) = cancel_case("cancel-no-auto", false, Some(2), CANCEL_EFFECT).await;
        let error = result.unwrap_err();
        assert!(matches!(error, DeployProfileError::WaitExit(_)), "{error}");
        assert!(!error.rolled_back());
        assert!(base.join("finished").exists());
    }

    #[tokio::test]
    async fn cancel_with_hanging_activation_has_typed_timeout_not_rollback() {
        let body = CANCEL_EFFECT.replace("sleep 0.1", "sleep 60");
        let start = std::time::Instant::now();
        let (result, _, base) = cancel_case("cancel-timeout", true, Some(1), &body).await;
        let error = result.unwrap_err();
        assert!(
            matches!(
                error,
                DeployProfileError::WaitActivationTimeout {
                    timeout_secs: 1,
                    ..
                }
            ),
            "{error}"
        );
        assert!(!error.rolled_back());
        assert!(!base.join("finished").exists());
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }

    #[tokio::test]
    async fn failed_waiter_with_successful_activation_is_not_claimed_as_rollback() {
        let body = CANCEL_EFFECT.replace(
            "touch \"$TEST_ROOT/finished\"\n    exit 1",
            "touch \"$TEST_ROOT/finished\"\n    exit 0",
        );
        let (result, _, _) = cancel_case("cancel-activation-success", true, Some(2), &body).await;
        let error = result.unwrap_err();
        assert!(matches!(error, DeployProfileError::WaitExit(_)), "{error}");
        assert!(!error.rolled_back());
    }

    #[tokio::test]
    async fn stale_cancel_and_canary_are_removed_before_new_attempt() {
        let (result, _, base) = cancel_case("cancel-stale", true, Some(2), CANCEL_EFFECT).await;
        assert!(matches!(
            result,
            Err(DeployProfileError::WaitRollback { .. })
        ));
        assert!(base.join("clean-start").exists());
        assert!(
            !base
                .join("deploy-rs-canary-00000000000000000000000000000000")
                .exists()
        );
    }

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
            "env -u NIXOS_NO_CHECK /nix/store/example-profile/activate-rs activate '/nix/store/example-profile' --profile-user root --profile-name system --temp-path '/tmp' --confirm-timeout 30 --magic-rollback --auto-rollback --dry-activate"
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

    #[tokio::test]
    async fn preflight_feature_detection_acceptance_refusal_and_transport_are_typed() {
        let (legacy, legacy_trace, _) = disposition_case(
            "legacy",
            "case \"$*\" in *switch-inhibitors*) exit 1 ;; esac\nexit 99",
            false,
            false,
        )
        .await;
        assert_eq!(legacy.unwrap(), ActivationDisposition::LiveSwitch);
        assert!(!legacy_trace.contains("switch-to-configuration check"));

        let (safe, safe_trace, _) = disposition_case(
            "safe",
            "case \"$*\" in *switch-inhibitors*) exit 0 ;; *'switch-to-configuration check'*) exit 0 ;; esac\nexit 99",
            false,
            false,
        )
        .await;
        assert_eq!(safe.unwrap(), ActivationDisposition::LiveSwitch);
        assert!(safe_trace.contains("switch-to-configuration check"));
        assert_eq!(safe_trace.matches("env -u NIXOS_NO_CHECK").count(), 1);

        let (refused, _, messages) = disposition_case(
            "refused",
            "case \"$*\" in *switch-inhibitors*) exit 0 ;; *'switch-to-configuration check'*) echo 'dbus inhibitor refused' >&2; exit 42 ;; esac\nexit 99",
            true,
            false,
        )
        .await;
        assert_eq!(
            refused.unwrap(),
            ActivationDisposition::DryPreview {
                reboot_pending: true
            }
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("dbus inhibitor refused"))
        );

        let (transport, _, _) = disposition_case(
            "transport",
            "case \"$*\" in *switch-inhibitors*) exit 0 ;; *'switch-to-configuration check'*) exit 255 ;; esac\nexit 99",
            false,
            false,
        )
        .await;
        assert!(matches!(
            transport,
            Err(DeployProfileError::PreflightTransport(_))
        ));
    }

    #[tokio::test]
    async fn explicit_boot_bypasses_preflight_and_dry_preview_is_non_mutating() {
        let (disposition, trace, _) =
            disposition_case("explicit-boot", "exit 99", true, true).await;
        assert_eq!(
            disposition.unwrap(),
            ActivationDisposition::DryPreview {
                reboot_pending: true
            }
        );
        assert!(trace.is_empty());
    }

    #[tokio::test]
    async fn boot_staging_success_uses_only_profile_set_and_boot_action() {
        let body = r#"case "$*" in
  *'readlink -e '*) echo /nix/store/11111111111111111111111111111111-old-system; exit 0 ;;
  *'nix-env --profile '*'-new-system'*) exit 0 ;;
  *'-new-system/bin/switch-to-configuration boot'*) exit 0 ;;
esac
exit 99"#;
        let (result, trace, _) = boot_stage_case("boot-success", body).await;
        result.unwrap();
        assert!(trace.contains("nix-env --profile /nix/var/nix/profiles/system --set /nix/store/00000000000000000000000000000000-new-system"));
        assert!(trace.contains("-new-system/bin/switch-to-configuration boot"));
        assert!(!trace.contains("switch-to-configuration switch"));
        assert!(!trace.contains("switch-to-configuration test"));
        assert!(!trace.contains("activate-rs"));
    }

    #[tokio::test]
    async fn boot_staging_failure_restores_profile_and_boot_default_without_live_activation() {
        let body = r#"case "$*" in
  *'readlink -e '*) echo /nix/store/11111111111111111111111111111111-old-system; exit 0 ;;
  *'nix-env --profile '*'-new-system'*) exit 0 ;;
  *'-new-system/bin/switch-to-configuration boot'*) echo new-boot-failed >&2; exit 23 ;;
  *'nix-env --profile '*'-old-system'*) exit 0 ;;
  *'-old-system/bin/switch-to-configuration boot'*) exit 0 ;;
esac
exit 99"#;
        let (result, trace, messages) = boot_stage_case("boot-recovery", body).await;
        let error = result.unwrap_err().to_string();
        assert!(error.contains("boot-stage-default exited"));
        assert!(error.contains("restored previous profile and boot default"));
        assert!(
            messages
                .iter()
                .any(|message| message.contains("new-boot-failed"))
        );
        assert!(trace.contains("--set /nix/store/11111111111111111111111111111111-old-system"));
        assert!(trace.contains("-old-system/bin/switch-to-configuration boot"));
        assert!(!trace.contains("switch-to-configuration switch"));
        assert!(!trace.contains("activate-rs"));
    }

    #[tokio::test]
    async fn missing_previous_profile_fails_before_mutation_and_compound_failure_is_preserved() {
        let (missing, missing_trace, _) = boot_stage_case(
            "boot-missing-previous",
            "case \"$*\" in *'readlink -e '*) exit 1 ;; esac\nexit 99",
        )
        .await;
        assert!(matches!(
            missing,
            Err(DeployProfileError::PreviousProfileExit(_))
        ));
        assert!(!missing_trace.contains("nix-env --profile"));

        let compound_body = r#"case "$*" in
  *'readlink -e '*) echo /nix/store/11111111111111111111111111111111-old-system; exit 0 ;;
  *'nix-env --profile '*'-new-system'*) exit 0 ;;
  *'-new-system/bin/switch-to-configuration boot'*) exit 23 ;;
  *'nix-env --profile '*'-old-system'*) exit 24 ;;
  *'-old-system/bin/switch-to-configuration boot'*) exit 25 ;;
esac
exit 99"#;
        let (compound, _, _) = boot_stage_case("boot-compound", compound_body).await;
        let error = compound.unwrap_err().to_string();
        assert!(error.contains("boot-recovery-profile exited"));
        assert!(error.contains("boot-recovery-default exited"));
    }
}
