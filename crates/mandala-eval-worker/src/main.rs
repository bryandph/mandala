//! `mandala-eval-worker` — a persistent, crash-isolated Nix evaluator.
//!
//! A separate binary (not a thread) so an evaluator abort takes down only the
//! worker; `mandala-core` supervises and respawns it. It speaks newline-
//! delimited JSON over stdio:
//!
//! ```text
//! → {"id":1,"op":"aggregate","flake":"/path/to/flake"}
//! ← {"id":1,"ok":true,"value":{…}}
//!
//! → {"id":2,"op":"expected_toplevels","flake":"/p","members":["a","b","c"]}
//! ← {"id":2,"ok":true,"value":{"a":"/nix/store/…","b":{"error":"…"}}}
//!   // per member: its out-path, or {"error":…} when only that member's
//!   // configuration failed; absent when it has no nixosConfigurations entry
//!
//! → {"id":3,"op":"host_eval","flake":"/p","member":"a"}
//! ← {"id":3,"ok":true,"value":"/nix/store/…"}      // null if no such nixos host
//!
//! → {"id":4,"op":"ping"}
//! ← {"id":4,"ok":true}
//! ```
//!
//! Errors are returned in-band (`{"id":…,"ok":false,"error":"…"}`), never
//! raised — the client decides whether to retry or fall back to the
//! `nix eval --json` subprocess path.

mod nix;

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use nix::Evaluator;

#[derive(Deserialize)]
struct Request {
    id: u64,
    op: String,
    #[serde(default)]
    flake: String,
    #[serde(default)]
    member: Option<String>,
    #[serde(default)]
    members: Option<Vec<String>>,
}

#[derive(Serialize)]
struct Response {
    id: u64,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Json>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Response {
    fn ok(id: u64, value: Option<Json>) -> Self {
        Self {
            id,
            ok: true,
            value,
            error: None,
        }
    }
    fn err(id: u64, error: String) -> Self {
        Self {
            id,
            ok: false,
            value: None,
            error: Some(error),
        }
    }
}

/// The `expected_toplevels` wire value: one entry per evaluated member, an
/// out-path string or an `{"error": …}` object.
fn toplevels_value(toplevels: nix::Toplevels) -> Json {
    let mut obj: serde_json::Map<String, Json> = toplevels
        .paths
        .into_iter()
        .map(|(k, v)| (k, Json::String(v)))
        .collect();
    for (member, error) in toplevels.errors {
        obj.insert(member, serde_json::json!({ "error": error }));
    }
    Json::Object(obj)
}

fn handle(ev: &mut Evaluator, req: &Request) -> Response {
    match req.op.as_str() {
        "ping" => Response::ok(req.id, None),
        "aggregate" => match ev.aggregate(&req.flake) {
            Ok(v) => Response::ok(req.id, Some(v)),
            Err(e) => Response::err(req.id, e),
        },
        "host_eval" => {
            let Some(member) = req.member.as_deref() else {
                return Response::err(req.id, "host_eval requires 'member'".to_string());
            };
            match ev.host_toplevel(&req.flake, member) {
                Ok(v) => Response::ok(req.id, Some(v.map_or(Json::Null, Json::String))),
                Err(e) => Response::err(req.id, e),
            }
        }
        "expected_toplevels" | "toplevel" => {
            let members = req.members.clone().unwrap_or_default();
            match ev.expected_toplevels(&req.flake, &members) {
                Ok(toplevels) => Response::ok(req.id, Some(toplevels_value(toplevels))),
                Err(e) => Response::err(req.id, e),
            }
        }
        other => Response::err(req.id, format!("unknown op {other:?}")),
    }
}

/// One response frame. A value that fails to serialize must still produce a
/// well-formed reply carrying the request id: an empty line would read as a
/// transport failure and get a healthy worker killed.
fn encode(resp: &Response) -> String {
    serde_json::to_string(resp).unwrap_or_else(|e| {
        serde_json::to_string(&Response::err(
            resp.id,
            format!("response serialization failed: {e}"),
        ))
        .unwrap_or_else(|_| format!(r#"{{"id":{},"ok":false}}"#, resp.id))
    })
}

fn main() {
    // The Nix evaluator recurses on the C stack; a flake-parts / dendritic
    // aggregate blows past the default 8 MiB thread stack (the `nix` CLI runs
    // eval on an enlarged stack for the same reason). Run the whole worker —
    // init AND the request loop — on one big-stack thread so the GC-registered
    // init thread is also the only thread touching values (C API thread-safety).
    let worker = std::thread::Builder::new()
        .name("mandala-eval".to_string())
        .stack_size(512 * 1024 * 1024)
        .spawn(run)
        .expect("spawn eval thread");
    let code = worker.join().unwrap_or(1);
    std::process::exit(code);
}

fn run() -> i32 {
    let mut ev = match Evaluator::new() {
        Ok(ev) => ev,
        Err(e) => {
            eprintln!("mandala-eval-worker: init failed: {e}");
            return 1;
        }
    };

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => handle(&mut ev, &req),
            // No id to echo on a malformed frame — surface under id 0.
            Err(e) => Response::err(0, format!("bad request: {e}")),
        };
        if writeln!(out, "{}", encode(&resp)).is_err() {
            break;
        }
        let _ = out.flush();
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_parses_all_op_shapes() {
        let agg: Request =
            serde_json::from_str(r#"{"id":1,"op":"aggregate","flake":"/f"}"#).unwrap();
        assert_eq!(agg.id, 1);
        assert_eq!(agg.op, "aggregate");
        assert_eq!(agg.flake, "/f");

        let he: Request =
            serde_json::from_str(r#"{"id":2,"op":"host_eval","flake":"/f","member":"h"}"#).unwrap();
        assert_eq!(he.member.as_deref(), Some("h"));

        let et: Request =
            serde_json::from_str(r#"{"id":3,"op":"expected_toplevels","members":["a","b"]}"#)
                .unwrap();
        assert_eq!(
            et.members.as_deref(),
            Some(&["a".to_string(), "b".to_string()][..])
        );
        // `flake` is optional in the wire frame (defaults empty).
        assert_eq!(et.flake, "");
    }

    #[test]
    fn response_ok_omits_error_and_null_value() {
        let r = Response::ok(7, None);
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"id":7,"ok":true}"#);

        let v = Response::ok(8, Some(Json::String("/nix/store/x".into())));
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            r#"{"id":8,"ok":true,"value":"/nix/store/x"}"#
        );
    }

    #[test]
    fn toplevels_value_mixes_paths_and_member_errors() {
        let mut t = nix::Toplevels::default();
        t.paths.insert("a".into(), "/nix/store/a".into());
        t.errors.insert("b".into(), "boom".into());
        assert_eq!(
            serde_json::to_string(&toplevels_value(t)).unwrap(),
            r#"{"a":"/nix/store/a","b":{"error":"boom"}}"#
        );
    }

    #[test]
    fn encode_is_one_framed_reply() {
        let line = encode(&Response::ok(11, Some(Json::from(1))));
        assert_eq!(line, r#"{"id":11,"ok":true,"value":1}"#);
        assert!(!line.contains('\n'));
    }

    #[test]
    fn response_err_carries_message() {
        let r = Response::err(9, "boom".into());
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"id":9,"ok":false,"error":"boom"}"#
        );
    }
}
