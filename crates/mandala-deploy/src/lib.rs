//! Native Mandala deploy controller.
//!
//! `data.rs`, `push.rs`, and `deploy.rs` are derived from the bottom half of
//! serokell/deploy-rs at commit
//! `6d3087eedff75a715b40c0e124ba15d2dd7bec28` and retain MPL-2.0 headers.
//! The intentional patch is sink injection: process-global `log`
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
    /// Alternate `nix` executable used by effect-isolated tests.
    pub nix_program: Option<PathBuf>,
    /// Alternate `ssh` executable used by effect-isolated tests.
    pub ssh_program: Option<PathBuf>,
    /// Explicit child environment overrides used by effect-isolated tests.
    pub environment: Vec<(String, String)>,
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
    #[error("`sshUser` is not set for profile {0} of node {1}")]
    MissingSshUser(String, String),
    #[error("Neither `user` nor `sshUser` are set for profile {0} of node {1}")]
    NoProfileUser(String, String),
}

impl DeployData<'_> {
    pub fn defs(&self) -> Result<DeployDefs, DeployDataDefsError> {
        let ssh_user = self.merged_settings.ssh_user.clone().ok_or_else(|| {
            DeployDataDefsError::MissingSshUser(
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

    /// Exact SSH client arguments derived from the flattened contract.
    /// Connection scalars precede free-form options so ssh's first-value-wins
    /// behavior keeps the declared endpoint settings authoritative.
    pub(crate) fn ssh_args(&self) -> Vec<String> {
        let mut args = vec![
            "-p".to_owned(),
            self.merged_settings.ssh_port.unwrap_or(22).to_string(),
        ];
        if let Some(identity_file) = &self.merged_settings.identity_file {
            args.extend([
                "-i".to_owned(),
                identity_file.display().to_string(),
                "-o".to_owned(),
                "IdentitiesOnly=yes".to_owned(),
                "-o".to_owned(),
                "IdentityAgent=none".to_owned(),
            ]);
        }
        args.extend(self.merged_settings.ssh_opts.iter().cloned());
        args
    }

    /// Nix accepts SSH options through one shell-tokenized environment
    /// string. Quote only arguments that need it so ordinary traces stay
    /// readable while identity paths containing spaces remain one token.
    pub(crate) fn nix_ssh_opts(&self) -> String {
        self.ssh_args()
            .iter()
            .map(|arg| shell_word(arg))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn shell_word(value: &str) -> String {
    if !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':' | b'=' | b'@')
        })
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::data::{GenericSettings, Node, NodeSettings, Profile, ProfileSettings};

    #[derive(Default)]
    struct NullSink;

    impl EventSink for NullSink {
        fn emit(&self, _level: Level, _message: &str) {}
    }

    fn fixture(settings: GenericSettings) -> (Node, Profile) {
        let profile = Profile {
            profile_settings: ProfileSettings {
                path: "/nix/store/example-profile".into(),
                profile_path: None,
            },
            generic_settings: GenericSettings::default(),
        };
        let node = Node {
            generic_settings: settings,
            node_settings: NodeSettings {
                hostname: "declared.example".into(),
                profiles: HashMap::from([("system".into(), profile.clone())]),
                profiles_order: vec![],
            },
        };
        (node, profile)
    }

    #[test]
    fn declared_connection_is_complete_and_does_not_need_ambient_user() {
        let (node, profile) = fixture(GenericSettings {
            ssh_user: Some("declared-user".into()),
            ssh_port: Some(2222),
            identity_file: Some("/keys/declared identity".into()),
            ..GenericSettings::default()
        });
        let overrides = CmdOverrides::default();
        let sink = NullSink;
        let deploy = make_deploy_data(
            &GenericSettings::default(),
            &node,
            "declared",
            &profile,
            "system",
            &overrides,
            &sink,
        );

        assert_eq!(deploy.defs().unwrap().ssh_user, "declared-user");
        assert_eq!(
            deploy.ssh_args(),
            [
                "-p",
                "2222",
                "-i",
                "/keys/declared identity",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "IdentityAgent=none",
            ]
        );
        assert_eq!(
            deploy.nix_ssh_opts(),
            "-p 2222 -i '/keys/declared identity' -o IdentitiesOnly=yes -o IdentityAgent=none"
        );
    }

    #[test]
    fn missing_ssh_user_is_an_error_instead_of_reading_process_user() {
        let (node, profile) = fixture(GenericSettings::default());
        let overrides = CmdOverrides::default();
        let sink = NullSink;
        let deploy = make_deploy_data(
            &GenericSettings::default(),
            &node,
            "missing-user",
            &profile,
            "system",
            &overrides,
            &sink,
        );

        assert!(matches!(
            deploy.defs(),
            Err(DeployDataDefsError::MissingSshUser(_, _))
        ));
    }
}
