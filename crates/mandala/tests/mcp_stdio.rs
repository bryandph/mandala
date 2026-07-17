//! Scripted stdio MCP handshake against the built `mandala mcp` binary
//! (OpenSpec change `mandala-rust-rewrite`, spike 1.2).
//!
//! Drives the full initialize → notifications/initialized → tools/list →
//! tools/call → clean-exit sequence over newline-delimited JSON-RPC, exactly
//! as a headless MCP client (Claude Code's stdio transport) does. Proves the
//! rust-mcp-sdk 0.10 stdio server negotiates, advertises the `resolve` tool,
//! answers a call with structured JSON, and exits 0 when stdin closes.
//!
//! Interactive validation from a real Claude Code session is an OPERATOR
//! CHECKPOINT (it needs a live agent); this test is the automatable half.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Read newline-delimited JSON-RPC messages until one carries the given id,
/// skipping any unsolicited notifications/logs. Panics on EOF.
fn read_response(reader: &mut impl BufRead, id: i64) -> serde_json::Value {
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).expect("read stdout");
        assert!(n != 0, "server closed stdout before responding to id {id}");
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("non-JSON line {line:?}: {e}"));
        if msg.get("id").and_then(serde_json::Value::as_i64) == Some(id) {
            return msg;
        }
    }
}

#[test]
fn stdio_handshake_lists_and_calls_resolve() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mandala"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mandala mcp");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let send = |stdin: &mut std::process::ChildStdin, v: &serde_json::Value| {
        stdin.write_all(v.to_string().as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    };

    // 1. initialize
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "spike-driver", "version": "0"}
            }
        }),
    );
    let init = read_response(&mut stdout, 1);
    assert_eq!(
        init["result"]["serverInfo"]["name"], "mandala-fleet",
        "initialize result: {init}"
    );

    // 2. initialized notification (no response)
    send(
        &mut stdin,
        &serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );

    // 3. tools/list — the resolve tool is advertised with its schema.
    send(
        &mut stdin,
        &serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    let tools = read_response(&mut stdout, 2);
    let list = tools["result"]["tools"].as_array().expect("tools array");
    assert_eq!(list.len(), 1, "tools/list: {tools}");
    assert_eq!(list[0]["name"], "resolve");

    // 4. tools/call resolve — structured JSON round-trips back as text content.
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "resolve", "arguments": {"selector": "all,!@gateway"}}
        }),
    );
    let call = read_response(&mut stdout, 3);
    let text = call["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tools/call result: {call}"));
    let body: serde_json::Value = serde_json::from_str(text).expect("tool body is JSON");
    assert_eq!(body["members"], serde_json::json!(["cache", "web"]));
    assert_eq!(body["limit"], "cache,web");

    // 5. clean exit: closing stdin (EOF) must let the server shut down 0.
    drop(stdin);
    let status = wait_with_timeout(&mut child, Duration::from_secs(10));
    assert!(
        status.success(),
        "server exited non-zero on stdin close: {status:?}"
    );
}

/// Poll for process exit up to `timeout`, killing (and failing) if it hangs —
/// a clean exit on stdin EOF is exactly what this asserts.
fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> std::process::ExitStatus {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => return status,
            None if start.elapsed() > timeout => {
                let _ = child.kill();
                panic!("server did not exit within {timeout:?} after stdin close");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}
