// SPDX-FileCopyrightText: 2020 Serokell <https://serokell.io/>
//
// SPDX-License-Identifier: MPL-2.0
//
// Vendored from serokell/deploy-rs@6d3087eedff75a715b40c0e124ba15d2dd7bec28.
// Spike patch: accept a prebuilt profile and emit through DeployData::sink.

use std::process::Stdio;

use thiserror::Error;
use tokio::process::Command;

use crate::event::{Level, emit_output};
use crate::{DeployData, DeployDefs};

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
    let mut command = Command::new("nix");
    command.arg("copy");
    if deploy_data.merged_settings.fast_connection != Some(true) {
        command.arg("--substitute-on-destination");
    }
    if !check_sigs {
        command.arg("--no-check-sigs");
    }
    command
        .arg("--to")
        .arg(format!("ssh://{}@{hostname}", deploy_defs.ssh_user))
        .arg(&deploy_data.profile.profile_settings.path)
        .env(
            "NIX_SSHOPTS",
            deploy_data.merged_settings.ssh_opts.join(" "),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = command.output().await?;
    emit_output(deploy_data.sink, &output);
    if !output.status.success() {
        return Err(PushProfileError::Exit(output.status));
    }
    deploy_data.sink.emit(Level::Info, "copy-complete");
    Ok(())
}
