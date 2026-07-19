//! Scripted stdio MCP handshake against the built `mandala mcp` binary
//! (OpenSpec change `mandala-rust-rewrite`; spike 1.2, extended by section 4).
//!
//! Drives the full initialize → notifications/initialized → tools/list →
//! tools/call → clean-exit sequence over newline-delimited JSON-RPC, exactly
//! as a headless MCP client (Claude Code's stdio transport) does. Proves the
//! rust-mcp-sdk 0.10 stdio server negotiates, advertises the full 12-tool
//! surface, answers a call with structured JSON, and exits 0 when stdin
//! closes. The fleet is injected via the `MANDALA_AGGREGATE_FILE` seam (the
//! same aggregate the parity fixtures use), so no flake eval runs; state is
//! isolated via `MANDALA_FLEET_STATE`.
//!
//! Interactive validation from a real Claude Code session is an OPERATOR
//! CHECKPOINT (it needs a live agent); this test is the automatable half.
//!
//! Since `mandala-native-tui` section 3 the server is context-aware: each
//! test gives its child processes their own working directory (the context
//! identity is scoped to the canonical flake path — the cwd for the default
//! `--flake .`) and its own `MANDALA_FLEET_STATE`, so concurrently running
//! tests can never join each other's contexts.

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

/// A per-test scratch tree: a `flake/` working directory (the context scope),
/// a `state/` dir, and the injected aggregate. Unique per test AND per run.
fn scratch_tree(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let scratch = std::env::temp_dir().join(format!(
        "mandala-mcp-stdio-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let flake = scratch.join("flake");
    let state = scratch.join("state");
    std::fs::create_dir_all(&flake).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    let aggregate = scratch.join("aggregate.json");
    std::fs::write(
        &aggregate,
        serde_json::json!({
            "schemaVersion": 1,
            "members": {
                "web": {"name": "web"},
                "cache": {"name": "cache"},
                "router": {"name": "router"},
            },
            "groups": {"k3s": ["cache", "web"], "gateway": ["router"]},
            "projections": {"deploy": {"nodes": ["cache", "web"]}},
        })
        .to_string(),
    )
    .unwrap();
    (flake, state, aggregate)
}

/// Spawn one `mandala mcp` instance scoped to the scratch tree.
fn spawn_mcp(
    flake: &std::path::Path,
    state: &std::path::Path,
    aggregate: &std::path::Path,
) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_mandala"))
        .arg("mcp")
        .current_dir(flake)
        .env("MANDALA_AGGREGATE_FILE", aggregate)
        .env("MANDALA_FLEET_STATE", state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mandala mcp")
}

fn send(stdin: &mut std::process::ChildStdin, v: &serde_json::Value) {
    stdin.write_all(v.to_string().as_bytes()).unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();
}

/// The one discovery file under a scratch state dir (`None` when released).
fn read_discovery(state: &std::path::Path) -> Option<serde_json::Value> {
    let dir = state.join("mcp").join("contexts");
    let entries = std::fs::read_dir(&dir).ok()?;
    for entry in entries.flatten() {
        if entry.path().extension().is_some_and(|e| e == "json") {
            let text = std::fs::read_to_string(entry.path()).ok()?;
            return serde_json::from_str(&text).ok();
        }
    }
    None
}

#[test]
fn stdio_handshake_lists_and_calls_resolve() {
    let (flake, state, aggregate) = scratch_tree("handshake");
    let mut child = spawn_mcp(&flake, &state, &aggregate);

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

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

    // 3. tools/list — the full 12-tool surface, in the Python server's
    // registration order (the fleet-mcp parity contract).
    send(
        &mut stdin,
        &serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    let tools = read_response(&mut stdout, 2);
    let list = tools["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = list
        .iter()
        .map(|t| t["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        names,
        [
            "members",
            "groups",
            "resolve",
            "ping",
            "host_eval",
            "drift",
            "reload",
            "deploy_status",
            "build",
            "deploy",
            "restart_service",
            "reboot",
        ],
        "tools/list: {tools}"
    );

    // 4. tools/call resolve — structured JSON round-trips back as text content
    // (and as structuredContent), resolved from the injected aggregate.
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
    assert_eq!(
        call["result"]["structuredContent"]["limit"], "cache,web",
        "structuredContent mirrors the text body: {call}"
    );

    // 5. a gated action refuses without confirm — through the real transport.
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {"name": "deploy",
                       "arguments": {"selector": "@k3s", "dry_activate": false}}
        }),
    );
    let refusal = read_response(&mut stdout, 4);
    let body = &refusal["result"]["structuredContent"];
    assert_eq!(body["refused"], true, "deploy refusal: {refusal}");
    assert_eq!(body["required_confirm"], "cache,web");

    // 6. clean exit: closing stdin (EOF) must let the server shut down 0 —
    // and, as the context leader, release its discovery claim on the way out
    // (orderly shutdown, not a crash).
    drop(stdin);
    let status = wait_with_timeout(&mut child, Duration::from_secs(10));
    assert!(
        status.success(),
        "server exited non-zero on stdin close: {status:?}"
    );
    assert!(
        read_discovery(&state).is_none(),
        "a leader's clean exit must release its discovery claim"
    );
}

/// The fleet-mcp "two harnesses share one context" scenario end-to-end, plus
/// the 3.1 stdio lifecycle: two real `mandala mcp` processes against one
/// checkout — the first leads, the second serves its own stdio conversation
/// (identical static tool list) while forwarding execution; when the leader's
/// stdin closes it shuts the context down in order and exits, and the
/// follower PROMOTES — its next call succeeds and the discovery file records
/// it as the new leader. Its stdio client noticed nothing.
#[test]
fn second_instance_proxies_and_promotes_when_the_leader_exits() {
    let (flake, state, aggregate) = scratch_tree("promote");

    // Instance A: leads (first against the checkout).
    let mut a = spawn_mcp(&flake, &state, &aggregate);
    let mut a_in = a.stdin.take().unwrap();
    let mut a_out = BufReader::new(a.stdout.take().unwrap());
    handshake(&mut a_in, &mut a_out);
    let d = read_discovery(&state).expect("the first instance published discovery");
    assert_eq!(
        d["pid"].as_u64(),
        Some(u64::from(a.id())),
        "instance A claimed the context: {d}"
    );

    // Instance B: joins as a follower — same checkout, same context.
    let mut b = spawn_mcp(&flake, &state, &aggregate);
    let mut b_in = b.stdin.take().unwrap();
    let mut b_out = BufReader::new(b.stdout.take().unwrap());
    handshake(&mut b_in, &mut b_out);
    let d = read_discovery(&state).expect("discovery still published");
    assert_eq!(
        d["pid"].as_u64(),
        Some(u64::from(a.id())),
        "instance A still leads: {d}"
    );

    // Both serve the identical static tool list over their own stdio pipes.
    let list_a = tools_list(&mut a_in, &mut a_out);
    let list_b = tools_list(&mut b_in, &mut b_out);
    assert_eq!(list_a, list_b, "the tool surface is role-independent");

    // The same call through both instances: identical results (execution
    // flows through the one leader either way).
    let via_a = call_resolve(&mut a_in, &mut a_out, 10);
    let via_b = call_resolve(&mut b_in, &mut b_out, 10);
    assert_eq!(
        via_a["result"]["structuredContent"], via_b["result"]["structuredContent"],
        "a proxied call matches a leader-served call"
    );
    assert_eq!(
        via_b["result"]["structuredContent"]["limit"], "cache,web",
        "and it is the real tool result: {via_b}"
    );

    // The leader's stdin closes: orderly context shutdown (drain, guarded
    // release), then exit 0 — so the follower CAN promote.
    drop(a_in);
    let status = wait_with_timeout(&mut a, Duration::from_secs(10));
    assert!(status.success(), "leader exit on stdin close: {status:?}");
    assert!(
        read_discovery(&state).is_none(),
        "the leader released its claim on the way out"
    );

    // The follower's next call fails over: it re-races, promotes, and the
    // (idempotent) read is retried on its own fresh handler — the stdio
    // client sees only a normal, correct response.
    let after = call_resolve(&mut b_in, &mut b_out, 11);
    assert_eq!(
        after["result"]["structuredContent"]["limit"], "cache,web",
        "the promoted follower answers the same call: {after}"
    );
    let d = read_discovery(&state).expect("the promoted follower re-published discovery");
    assert_eq!(
        d["pid"].as_u64(),
        Some(u64::from(b.id())),
        "instance B now leads: {d}"
    );

    // And the promoted leader exits cleanly too.
    drop(b_in);
    let status = wait_with_timeout(&mut b, Duration::from_secs(10));
    assert!(status.success(), "follower exit on stdin close: {status:?}");
    assert!(
        read_discovery(&state).is_none(),
        "the promoted leader released its claim on the way out"
    );
}

/// A real process-boundary failover: MCP launches a deploy, its leader is
/// SIGKILLed, the independent supervisor keeps the playbook/log/event stream
/// alive and settles meta, then a new leader attaches to the same run.
#[test]
fn deploy_supervisor_survives_launcher_process_death() {
    use std::os::unix::fs::PermissionsExt;

    let (flake, state, aggregate) = scratch_tree("deploy-orphan");
    let fake_bin = state.join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let playbook = fake_bin.join("ansible-playbook");
    std::fs::write(
        &playbook,
        r#"#!/bin/sh
set -eu
printf 'deploy-started\n'
printf '{"v":2,"ts":1,"host":"web","plugin":"deploy","event":"milestone","milestone":"eval"}\n' >> "$MANDALA_FLEET_EVENTS/web.jsonl"
sleep 2
printf 'deploy-finished\n'
printf '{"v":2,"ts":2,"host":"web","plugin":"deploy","event":"milestone","milestone":"confirm"}\n' >> "$MANDALA_FLEET_EVENTS/web.jsonl"
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&playbook).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&playbook, permissions).unwrap();
    let path = std::env::join_paths(
        std::iter::once(fake_bin.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();

    let spawn = || {
        Command::new(env!("CARGO_BIN_EXE_mandala"))
            .arg("mcp")
            .current_dir(&flake)
            .env("PATH", &path)
            .env("MANDALA_AGGREGATE_FILE", &aggregate)
            .env("MANDALA_FLEET_STATE", &state)
            .env(
                "MANDALA_DEPLOY_SUPERVISOR_BIN",
                env!("CARGO_BIN_EXE_mandala-run-supervisor"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mandala mcp")
    };

    let mut launcher = spawn();
    let mut launcher_in = launcher.stdin.take().unwrap();
    let mut launcher_out = BufReader::new(launcher.stdout.take().unwrap());
    handshake(&mut launcher_in, &mut launcher_out);
    send(
        &mut launcher_in,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {
                "name": "deploy",
                "arguments": {"selector": "web", "dry_activate": true}
            }
        }),
    );
    let launch = read_response(&mut launcher_out, 10);
    let launched = &launch["result"]["structuredContent"];
    assert_eq!(launched["ok"], true, "deploy launch: {launch}");
    let run_id = launched["run_id"].as_str().expect("run id").to_string();
    let run_dir =
        std::path::PathBuf::from(launched["events_dir"].as_str().expect("events directory"));

    launcher.kill().expect("SIGKILL launcher");
    launcher.wait().expect("reap killed launcher");
    drop(launcher_in);
    drop(launcher_out);

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let terminal_meta = loop {
        let meta = mandala_core::registry::read_meta(&run_dir);
        if meta.get("rc").is_some() {
            break meta;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "supervisor did not settle meta: {meta:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(terminal_meta["rc"], 0);
    assert!(terminal_meta["finished_at"].is_number());
    let child_pid = terminal_meta["pid"].as_i64();
    assert!(
        !mandala_core::registry::pid_alive(child_pid),
        "deploy child was not reaped"
    );
    let log = std::fs::read_to_string(run_dir.join("output.log")).unwrap();
    assert!(
        log.contains("deploy-started") && log.contains("deploy-finished"),
        "{log}"
    );
    let events = std::fs::read_to_string(run_dir.join("web.jsonl")).unwrap();
    assert!(events.contains("\"milestone\":\"confirm\""), "{events}");

    let mut promoted = spawn();
    let mut promoted_in = promoted.stdin.take().unwrap();
    let mut promoted_out = BufReader::new(promoted.stdout.take().unwrap());
    handshake(&mut promoted_in, &mut promoted_out);
    send(
        &mut promoted_in,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": {"name": "deploy_status", "arguments": {"run_id": run_id}}
        }),
    );
    let status = read_response(&mut promoted_out, 11);
    assert_eq!(
        status["result"]["structuredContent"]["liveness"], "finished",
        "{status}"
    );
    drop(promoted_in);
    let exit = wait_with_timeout(&mut promoted, Duration::from_secs(10));
    assert!(exit.success(), "promoted leader exit: {exit:?}");
}

/// Drive initialize + the initialized notification.
fn handshake(stdin: &mut std::process::ChildStdin, stdout: &mut impl BufRead) {
    send(
        stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "stdio-driver", "version": "0"}
            }
        }),
    );
    let init = read_response(stdout, 1);
    assert_eq!(init["result"]["serverInfo"]["name"], "mandala-fleet");
    send(
        stdin,
        &serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
}

/// The advertised tool names.
fn tools_list(stdin: &mut std::process::ChildStdin, stdout: &mut impl BufRead) -> Vec<String> {
    send(
        stdin,
        &serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    let tools = read_response(stdout, 2);
    tools["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// One `resolve @k3s` round-trip.
fn call_resolve(
    stdin: &mut std::process::ChildStdin,
    stdout: &mut impl BufRead,
    id: i64,
) -> serde_json::Value {
    send(
        stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": "resolve", "arguments": {"selector": "@k3s"}}
        }),
    );
    read_response(stdout, id)
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
