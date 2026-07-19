//! Native Mandala deploy-controller spike.
//!
//! `data.rs`, `push.rs`, and `deploy.rs` are derived from the bottom half of
//! serokell/deploy-rs at commit
//! `6d3087eedff75a715b40c0e124ba15d2dd7bec28` and retain MPL-2.0 headers.
//! The spike's intentional patch is sink injection: process-global `log`
//! records and inherited child output are replaced by a caller-owned,
//! per-host [`EventSink`]. No flake evaluation exists in this crate.

use std::path::{Path, PathBuf};

use thiserror::Error;

pub mod data;
pub mod deploy;
pub mod event;
pub mod push;

pub use event::{EventSink, JsonlSink, Level};

#[derive(Debug, Clone, Default)]
pub struct CmdOverrides {
    pub hostname: Option<String>,
}

pub struct DeployData<'a> {
    pub node_name: &'a str,
    pub node: &'a data::Node,
    pub profile_name: &'a str,
    pub profile: &'a data::Profile,
    pub cmd_overrides: &'a CmdOverrides,
    pub merged_settings: data::GenericSettings,
    pub sink: &'a dyn EventSink,
}

#[derive(Debug)]
pub struct DeployDefs {
    pub ssh_user: String,
    pub profile_user: String,
    pub sudo: Option<String>,
}

pub(crate) enum ProfileInfo {
    ProfilePath {
        profile_path: String,
    },
    ProfileUserAndName {
        profile_user: String,
        profile_name: String,
    },
}

#[derive(Error, Debug)]
pub enum DeployDataDefsError {
    #[error("Neither `user` nor `sshUser` are set for profile {0} of node {1}")]
    NoProfileUser(String, String),
}

impl DeployData<'_> {
    pub fn defs(&self) -> Result<DeployDefs, DeployDataDefsError> {
        let ssh_user = self
            .merged_settings
            .ssh_user
            .clone()
            .or_else(|| std::env::var("USER").ok())
            .ok_or_else(|| {
                DeployDataDefsError::NoProfileUser(
                    self.profile_name.to_owned(),
                    self.node_name.to_owned(),
                )
            })?;
        let profile_user = self.profile_user()?;
        let sudo = self
            .merged_settings
            .user
            .as_ref()
            .filter(|user| *user != &ssh_user)
            .map(|user| {
                format!(
                    "{} {}",
                    self.merged_settings.sudo.as_deref().unwrap_or("sudo -u"),
                    user
                )
            });
        Ok(DeployDefs {
            ssh_user,
            profile_user,
            sudo,
        })
    }

    fn profile_user(&self) -> Result<String, DeployDataDefsError> {
        self.merged_settings
            .user
            .clone()
            .or_else(|| self.merged_settings.ssh_user.clone())
            .ok_or_else(|| {
                DeployDataDefsError::NoProfileUser(
                    self.profile_name.to_owned(),
                    self.node_name.to_owned(),
                )
            })
    }

    pub(crate) fn profile_info(&self) -> Result<ProfileInfo, DeployDataDefsError> {
        match &self.profile.profile_settings.profile_path {
            Some(profile_path) => Ok(ProfileInfo::ProfilePath {
                profile_path: profile_path.clone(),
            }),
            None => Ok(ProfileInfo::ProfileUserAndName {
                profile_user: self.profile_user()?,
                profile_name: self.profile_name.to_owned(),
            }),
        }
    }
}

pub fn make_deploy_data<'a>(
    top_settings: &data::GenericSettings,
    node: &'a data::Node,
    node_name: &'a str,
    profile: &'a data::Profile,
    profile_name: &'a str,
    cmd_overrides: &'a CmdOverrides,
    sink: &'a dyn EventSink,
) -> DeployData<'a> {
    let merged_settings = profile
        .generic_settings
        .clone()
        .with_fallback(&node.generic_settings)
        .with_fallback(top_settings);
    DeployData {
        node_name,
        node,
        profile_name,
        profile,
        cmd_overrides,
        merged_settings,
        sink,
    }
}

#[must_use]
pub fn make_lock_path(temp_path: &Path, closure: &str) -> PathBuf {
    let hash = closure
        .strip_prefix("/nix/store/")
        .unwrap_or(closure)
        .split('-')
        .next()
        .unwrap_or(closure);
    temp_path.join(format!("deploy-rs-canary-{hash}"))
}
