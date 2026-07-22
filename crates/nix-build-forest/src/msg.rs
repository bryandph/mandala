//! Serde model for Nix's internal-JSON logger.
//!
//! Nix evolves this vocabulary. Unknown action/activity/result kinds and
//! additional fields therefore remain representable instead of making a live
//! build pane fail because a newer producer added data.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const NIX_JSON_PREFIX: &str = "@nix ";

/// Activity IDs from Nix `logging.hh`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActivityType {
    Unknown,
    CopyPath,
    FileTransfer,
    Realise,
    CopyPaths,
    Builds,
    Build,
    OptimiseStore,
    VerifyPaths,
    Substitute,
    QueryPathInfo,
    PostBuildHook,
    BuildWaiting,
    FetchTree,
    Other(i64),
}

impl ActivityType {
    #[must_use]
    pub fn from_code(code: i64) -> Self {
        match code {
            0 => Self::Unknown,
            100 => Self::CopyPath,
            101 => Self::FileTransfer,
            102 => Self::Realise,
            103 => Self::CopyPaths,
            104 => Self::Builds,
            105 => Self::Build,
            106 => Self::OptimiseStore,
            107 => Self::VerifyPaths,
            108 => Self::Substitute,
            109 => Self::QueryPathInfo,
            110 => Self::PostBuildHook,
            111 => Self::BuildWaiting,
            112 => Self::FetchTree,
            other => Self::Other(other),
        }
    }
}

/// Result IDs from Nix `logging.hh`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResultType {
    FileLinked,
    BuildLogLine,
    UntrustedPath,
    CorruptedPath,
    SetPhase,
    Progress,
    SetExpected,
    PostBuildLogLine,
    FetchStatus,
    Other(i64),
}

impl ResultType {
    #[must_use]
    pub fn from_code(code: i64) -> Self {
        match code {
            100 => Self::FileLinked,
            101 => Self::BuildLogLine,
            102 => Self::UntrustedPath,
            103 => Self::CorruptedPath,
            104 => Self::SetPhase,
            105 => Self::Progress,
            106 => Self::SetExpected,
            107 => Self::PostBuildLogLine,
            108 => Self::FetchStatus,
            other => Self::Other(other),
        }
    }
}

/// One tolerant internal-JSON record.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NixMessage {
    pub action: String,
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default, rename = "type")]
    pub type_code: Option<i64>,
    #[serde(default)]
    pub level: Option<i64>,
    #[serde(default)]
    pub parent: Option<u64>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub msg: Option<String>,
    #[serde(default)]
    pub raw_msg: Option<String>,
    #[serde(default)]
    pub fields: Vec<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl NixMessage {
    #[must_use]
    pub fn activity_type(&self) -> Option<ActivityType> {
        self.type_code.map(ActivityType::from_code)
    }

    #[must_use]
    pub fn result_type(&self) -> Option<ResultType> {
        self.type_code.map(ResultType::from_code)
    }

    #[must_use]
    pub fn message_text(&self) -> Option<&str> {
        self.raw_msg
            .as_deref()
            .or(self.msg.as_deref())
            .or(self.text.as_deref())
    }
}

/// `Ok(None)` means a normal non-`@nix` line. Malformed internal JSON is an
/// error so the state machine can count it without terminating the stream.
pub fn parse_nix_line(line: &str) -> Result<Option<NixMessage>, serde_json::Error> {
    let Some(payload) = line.trim_end().strip_prefix(NIX_JSON_PREFIX) else {
        return Ok(None);
    };
    serde_json::from_str(payload).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_and_future_protocol_values_parse() {
        let build = parse_nix_line(
            r#"@nix {"action":"start","id":7,"type":105,"fields":["/nix/store/a.drv","",1,1],"future":true}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(build.activity_type(), Some(ActivityType::Build));
        assert_eq!(build.extra["future"], Value::Bool(true));

        let future = parse_nix_line(r#"@nix {"action":"start","id":8,"type":999}"#)
            .unwrap()
            .unwrap();
        assert_eq!(future.activity_type(), Some(ActivityType::Other(999)));
    }

    #[test]
    fn plain_lines_are_not_protocol_errors() {
        assert!(
            parse_nix_line("warning: ordinary stderr")
                .unwrap()
                .is_none()
        );
    }
}
