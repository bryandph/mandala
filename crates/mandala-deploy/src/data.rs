// SPDX-FileCopyrightText: 2020 Serokell <https://serokell.io/>
//
// SPDX-License-Identifier: MPL-2.0
//
// Vendored from serokell/deploy-rs@6d3087eedff75a715b40c0e124ba15d2dd7bec28.
// The serde data model is intentionally kept structurally aligned with
// upstream. Mandala owns tier merging before constructing this model.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct GenericSettings {
    #[serde(rename = "sshUser")]
    pub ssh_user: Option<String>,
    #[serde(rename = "sshPort")]
    pub ssh_port: Option<u16>,
    #[serde(rename = "identityFile")]
    pub identity_file: Option<PathBuf>,
    pub user: Option<String>,
    #[serde(default, rename = "sshOpts")]
    pub ssh_opts: Vec<String>,
    #[serde(rename = "fastConnection")]
    pub fast_connection: Option<bool>,
    #[serde(rename = "autoRollback")]
    pub auto_rollback: Option<bool>,
    #[serde(rename = "confirmTimeout")]
    pub confirm_timeout: Option<u16>,
    #[serde(rename = "activationTimeout")]
    pub activation_timeout: Option<u16>,
    #[serde(rename = "tempPath")]
    pub temp_path: Option<PathBuf>,
    #[serde(rename = "magicRollback")]
    pub magic_rollback: Option<bool>,
    pub sudo: Option<String>,
    #[serde(default, rename = "remoteBuild")]
    pub remote_build: Option<bool>,
    #[serde(rename = "interactiveSudo")]
    pub interactive_sudo: Option<bool>,
}

impl GenericSettings {
    /// deploy-rs merge semantics: the inner value wins for scalars and its
    /// ssh options precede the outer value's options.
    #[must_use]
    pub fn with_fallback(mut self, outer: &Self) -> Self {
        macro_rules! fallback {
            ($field:ident) => {
                if self.$field.is_none() {
                    self.$field = outer.$field.clone();
                }
            };
        }
        fallback!(ssh_user);
        fallback!(ssh_port);
        fallback!(identity_file);
        fallback!(user);
        fallback!(fast_connection);
        fallback!(auto_rollback);
        fallback!(confirm_timeout);
        fallback!(activation_timeout);
        fallback!(temp_path);
        fallback!(magic_rollback);
        fallback!(sudo);
        fallback!(remote_build);
        fallback!(interactive_sudo);
        self.ssh_opts.extend(outer.ssh_opts.iter().cloned());
        self
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct NodeSettings {
    pub hostname: String,
    pub profiles: HashMap<String, Profile>,
    #[serde(default, rename = "profilesOrder")]
    pub profiles_order: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ProfileSettings {
    pub path: String,
    #[serde(rename = "profilePath")]
    pub profile_path: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Profile {
    #[serde(flatten)]
    pub profile_settings: ProfileSettings,
    #[serde(flatten)]
    pub generic_settings: GenericSettings,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Node {
    #[serde(flatten)]
    pub generic_settings: GenericSettings,
    #[serde(flatten)]
    pub node_settings: NodeSettings,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Data {
    #[serde(flatten)]
    pub generic_settings: GenericSettings,
    pub nodes: HashMap<String, Node>,
}
