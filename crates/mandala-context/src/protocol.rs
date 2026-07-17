//! Coordination protocol v1: newline-delimited JSON frames over the loopback
//! endpoint — the eval-worker precedent, not a second HTTP stack.
//!
//! ```text
//! → {"type":"hello","v":1,"token":"…","client":"claude-code","flake":"/p"}
//! ← {"type":"welcome","v":1,"flake":"/p","pid":4242}          // or:
//! ← {"type":"error","error":"unauthorized","flake":"/p"}      // then close
//!
//! → {"type":"call","id":7,"tool":"resolve","args":{"selector":"@k3s"}}
//! ← {"type":"heartbeat"}                  // while any call is in flight
//! ← {"type":"result","id":7,"ok":true,"result":{…}}
//!
//! → {"type":"subscribe"}
//! ← {"type":"subscribed"}
//! ← {"type":"event","event":{…}}          // server-pushed, unbounded
//!
//! → {"type":"ping"}
//! ← {"type":"pong"}
//! ```
//!
//! The hello is authenticated *before anything else is served*: a wrong token
//! gets exactly one structured `error` frame — which carries the server's
//! flake path so a port-collision probe can tell "another context squats my
//! derived port" (move on) from "my context, my token is stale" (re-read
//! discovery) — and the connection closes. Heartbeats exist so a
//! long-blocking proxied call (`deploy_status` waits up to 570s) keeps its
//! connection observably alive; they are flow, not protocol state, and either
//! side may ignore them.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The coordination protocol version carried in hello/welcome.
pub const PROTOCOL_VERSION: u32 = 1;

/// The unauthorized error text (matched by the probe logic; a contract).
pub const UNAUTHORIZED: &str = "unauthorized";

/// One line-frame of the coordination protocol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    /// Client → server, first frame: bearer auth + identity.
    Hello {
        v: u32,
        token: String,
        /// The originating client identity (labels this connection's calls in
        /// the activity stream).
        client: String,
        /// The flake path the client believes it is joining.
        flake: String,
    },
    /// Server → client: authenticated; the context's identity of record.
    Welcome { v: u32, flake: String, pid: u32 },
    /// Server → client: a connection-level failure. On auth failure `error`
    /// is [`UNAUTHORIZED`], `flake` identifies the server's context, and the
    /// connection closes right after.
    Error {
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        flake: Option<String>,
    },
    /// Client → server: execute one tool call.
    Call {
        id: u64,
        tool: String,
        #[serde(default)]
        args: serde_json::Map<String, Value>,
    },
    /// Server → client: the settled call `id`.
    #[serde(rename = "result")]
    CallResult {
        id: u64,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Client → server: start streaming activity events on this connection.
    Subscribe,
    /// Server → client: subscription acknowledged; events follow.
    Subscribed,
    /// Server → client: one activity event.
    Event { event: Value },
    /// Client → server: liveness probe.
    Ping,
    /// Server → client: liveness answer.
    Pong,
    /// Server → client: the connection is alive while a call blocks.
    Heartbeat,
}

impl Frame {
    /// Serialize to one protocol line (no trailing newline).
    ///
    /// # Errors
    /// `serde_json` failures (unrepresentable payload values).
    pub fn to_line(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }

    /// Parse one protocol line.
    ///
    /// # Errors
    /// The line is not a v1 frame.
    pub fn from_line(line: &str) -> serde_json::Result<Self> {
        serde_json::from_str(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frames_roundtrip_with_stable_tags() {
        let frames = [
            Frame::Hello {
                v: 1,
                token: "t".into(),
                client: "c".into(),
                flake: "/f".into(),
            },
            Frame::Welcome {
                v: 1,
                flake: "/f".into(),
                pid: 1,
            },
            Frame::Error {
                error: UNAUTHORIZED.into(),
                flake: Some("/f".into()),
            },
            Frame::Call {
                id: 7,
                tool: "resolve".into(),
                args: json!({"selector": "@k3s"}).as_object().cloned().unwrap(),
            },
            Frame::CallResult {
                id: 7,
                ok: true,
                result: Some(json!({"members": []})),
                error: None,
            },
            Frame::Subscribe,
            Frame::Subscribed,
            Frame::Event {
                event: json!({"tool": "resolve", "status": "start"}),
            },
            Frame::Ping,
            Frame::Pong,
            Frame::Heartbeat,
        ];
        for frame in frames {
            let line = frame.to_line().unwrap();
            assert!(!line.contains('\n'), "one frame, one line: {line}");
            assert_eq!(Frame::from_line(&line).unwrap(), frame);
        }
    }

    #[test]
    fn wire_tags_are_snake_case_type_fields() {
        let line = Frame::CallResult {
            id: 1,
            ok: false,
            result: None,
            error: Some("boom".into()),
        }
        .to_line()
        .unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["type"], "result");
        assert_eq!(value.get("result"), None, "None fields stay off the wire");
    }

    #[test]
    fn unknown_frames_are_parse_errors() {
        assert!(Frame::from_line("{\"type\":\"warp\"}").is_err());
        assert!(Frame::from_line("not json").is_err());
    }
}
