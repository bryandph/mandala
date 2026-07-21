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
    assert!(
        !state.join("runs").is_dir()
            || std::fs::read_dir(state.join("runs"))
                .unwrap()
                .next()
                .is_none(),
        "a refused call must create no registry run"
    );

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

/// D9.4 local-process drill: a follower forwards one mutation to the context
/// leader, the real native engine runs over shell-only Nix/SSH effects, and the
/// follower promotes + attaches the exact live run after the leader is killed.
#[test]
fn native_deploy_survives_leader_death_and_attaches_after_promotion() {
    use std::os::unix::fs::PermissionsExt;

    let (flake, state, aggregate) = scratch_tree("deploy-orphan");
    let setting = serde_json::json!({
        "activation": "switch",
        "hostname": "stub.invalid",
        "sshUser": "deployer",
        "sshOpts": [],
        "autoRollback": true,
        "fastConnection": false,
        "magicRollback": true,
        "confirmTimeout": 30,
        "activationTimeout": 60,
        "tempPath": "/tmp/mandala-drill",
        "sudo": null,
        "user": "root",
    });
    std::fs::write(
        &aggregate,
        serde_json::json!({
            "schemaVersion": 1,
            "members": {
                "cache": {
                    "name": "cache",
                    "deployment": {"deployRs": {"enable": true}},
                },
                "web": {
                    "name": "web",
                    "deployment": {"deployRs": {"enable": true}},
                },
            },
            "groups": {"k3s": ["cache", "web"]},
            "projections": {
                "deploy": {
                    "nodes": ["cache", "web"],
                    "settings": {"cache": setting, "web": setting},
                },
            },
        })
        .to_string(),
    )
    .unwrap();

    let fake_bin = state.join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let effects = state.join("effects.log");
    let nix = fake_bin.join("nix");
    std::fs::write(
        &nix,
        r#"#!/bin/sh
set -eu
case "$1" in
  build)
    printf 'build %s\n' "$*" >> "$MANDALA_FLEET_STATE/effects.log"
    out_link=
    while [ "$#" -gt 0 ]; do
      if [ "$1" = "--out-link" ]; then out_link=$2; shift 2; else shift; fi
    done
    ln -s /nix/store/00000000000000000000000000000000-cache-profile "$out_link"
    ln -s /nix/store/11111111111111111111111111111111-web-profile "$out_link-1"
    printf '%s\n' '@nix {"action":"start","id":1,"type":105,"fields":["/nix/store/drill.drv"]}' >&2
    sleep 4
    printf '%s\n' '@nix {"action":"stop","id":1}' >&2
    ;;
  copy)
    printf 'copy %s\n' "$*" >> "$MANDALA_FLEET_STATE/effects.log"
    printf 'copied %s\n' "$*"
    sleep 1
    ;;
  *) exit 90 ;;
esac
"#,
    )
    .unwrap();
    let ssh = fake_bin.join("ssh");
    std::fs::write(
        &ssh,
        r#"#!/bin/sh
set -eu
printf 'ssh %s\n' "$*" >> "$MANDALA_FLEET_STATE/effects.log"
printf 'activated %s\n' "$*"
sleep 1
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&nix).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&nix, permissions.clone()).unwrap();
    std::fs::set_permissions(&ssh, permissions).unwrap();
    let path = std::env::join_paths(
        std::iter::once(fake_bin.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();

    let spawn = || {
        let stderr = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(state.join("context.stderr"))
            .unwrap();
        Command::new(env!("CARGO_BIN_EXE_mandala"))
            .arg("--flake")
            .arg(&flake)
            .arg("mcp")
            .current_dir(&flake)
            .env("PATH", &path)
            .env("MANDALA_AGGREGATE_FILE", &aggregate)
            .env("MANDALA_FLEET_STATE", &state)
            .env_remove("MANDALA_DEPLOY_ENGINE_BIN")
            .env(
                "MANDALA_DEPLOY_SUPERVISOR_BIN",
                env!("CARGO_BIN_EXE_mandala-run-supervisor"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("spawn mandala mcp")
    };

    let mut leader = spawn();
    let mut leader_in = leader.stdin.take().unwrap();
    let mut leader_out = BufReader::new(leader.stdout.take().unwrap());
    handshake(&mut leader_in, &mut leader_out);
    let elected = read_discovery(&state).expect("leader published discovery");
    assert_eq!(elected["pid"].as_u64(), Some(u64::from(leader.id())));

    let mut follower = spawn();
    let mut follower_in = follower.stdin.take().unwrap();
    let mut follower_out = BufReader::new(follower.stdout.take().unwrap());
    handshake(&mut follower_in, &mut follower_out);
    let still_elected = read_discovery(&state).expect("leader discovery remains published");
    assert_eq!(
        still_elected["pid"].as_u64(),
        Some(u64::from(leader.id())),
        "the second MCP process must enter as a follower: {still_elected}"
    );

    // The mutation enters through the follower and executes once at the leader.
    send(
        &mut follower_in,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {
                "name": "deploy",
                "arguments": {"selector": "@k3s"}
            }
        }),
    );
    let launch = read_response(&mut follower_out, 10);
    let launched = &launch["result"]["structuredContent"];
    assert_eq!(launched["ok"], true, "deploy launch: {launch}");
    let run_id = launched["run_id"].as_str().expect("run id").to_string();
    let run_dir =
        std::path::PathBuf::from(launched["events_dir"].as_str().expect("events directory"));
    assert_eq!(
        run_dir.file_name().and_then(|name| name.to_str()),
        Some(run_id.as_str()),
        "MCP must return the engine-published run identity"
    );
    assert_eq!(
        std::fs::read_dir(state.join("runs")).unwrap().count(),
        1,
        "one forwarded mutation must create exactly one engine-owned run"
    );
    let published_meta = mandala_core::registry::read_meta(&run_dir);
    assert_eq!(published_meta["run_id"], run_id);
    assert_eq!(published_meta["limit"], "cache,web");
    assert_eq!(published_meta["dry_activate"], true);
    assert_eq!(published_meta["throttle"], 4);
    assert!(published_meta.get("rc").is_none(), "work must be in flight");

    // Publication precedes build. Wait until the real engine has entered the
    // shell-stub Nix effect before killing only the context leader.
    let in_flight_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !std::fs::read_to_string(&effects)
        .unwrap_or_default()
        .lines()
        .any(|line| line.starts_with("build "))
    {
        assert!(std::time::Instant::now() < in_flight_deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(mandala_core::registry::pid_alive(
        published_meta
            .get("pid")
            .and_then(serde_json::Value::as_i64)
    ));

    leader.kill().expect("SIGKILL context leader");
    leader.wait().expect("reap killed leader");
    drop(leader_in);
    drop(leader_out);

    // This idempotent read crosses the dead connection, promotes the existing
    // follower, retries once there, and attaches the exact live engine run.
    send(
        &mut follower_in,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": {
                "name": "deploy_status",
                "arguments": {"run_id": run_id, "wait_seconds": 0}
            }
        }),
    );
    let live = read_response(&mut follower_out, 11);
    let live = &live["result"]["structuredContent"];
    assert_eq!(live["run_id"], run_id);
    assert_eq!(
        live["liveness"], "running",
        "attached after promotion: {live}"
    );
    let promotion_deadline = std::time::Instant::now() + Duration::from_secs(2);
    let promoted = loop {
        if let Some(record) = read_discovery(&state) {
            break record;
        }
        assert!(
            std::time::Instant::now() < promotion_deadline,
            "promoted follower did not publish discovery"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        promoted["pid"].as_u64(),
        Some(u64::from(follower.id())),
        "the attached read must be served by the promoted follower: {promoted}"
    );

    send(
        &mut follower_in,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 12,
            "method": "tools/call",
            "params": {
                "name": "deploy_status",
                "arguments": {"run_id": run_id, "wait_seconds": 10}
            }
        }),
    );
    let settled = read_response(&mut follower_out, 12);
    let settled = &settled["result"]["structuredContent"];
    assert_eq!(settled["run_id"], run_id);
    assert_eq!(settled["liveness"], "finished", "settled run: {settled}");
    assert_eq!(settled["meta"]["rc"], 0);
    assert_eq!(settled["meta"]["process_rc"], 0);
    assert_eq!(
        settled["meta"]["summary"],
        serde_json::json!({"total": 2, "confirmed": 2, "failed": 0, "rolled_back": 0})
    );
    assert_eq!(settled["hosts"]["cache"]["state"], "confirmed");
    assert_eq!(settled["hosts"]["web"]["state"], "confirmed");
    assert_eq!(
        std::fs::read_dir(state.join("runs")).unwrap().count(),
        1,
        "failover must neither replay the mutation nor allocate a frontend run"
    );

    let terminal_meta = mandala_core::registry::read_meta(&run_dir);
    assert!(!mandala_core::registry::pid_alive(
        terminal_meta.get("pid").and_then(serde_json::Value::as_i64)
    ));
    let launch_root = state.join("deploy-launches");
    let launch = std::fs::read_dir(&launch_root)
        .unwrap()
        .next()
        .expect("one deploy coordination directory")
        .unwrap()
        .path();
    let log = std::fs::read_to_string(launch.join("output.log")).unwrap();
    assert!(log.contains("deploy summary: total=2 confirmed=2"), "{log}");
    let build = std::fs::read_to_string(run_dir.join("build.jsonl")).unwrap();
    assert!(build.contains("\"event\":\"nixlog\""), "{build}");
    for host in ["cache", "web"] {
        let stream = std::fs::read_to_string(run_dir.join(format!("{host}.jsonl"))).unwrap();
        assert!(stream.contains("\"milestone\":\"copy\""), "{stream}");
        assert!(stream.contains("\"milestone\":\"confirm\""), "{stream}");
    }

    let effects = std::fs::read_to_string(&effects).unwrap();
    assert_eq!(
        effects
            .lines()
            .filter(|line| line.starts_with("build "))
            .count(),
        1,
        "mutation must not retry its native build: {effects}"
    );
    assert_eq!(
        effects
            .lines()
            .filter(|line| line.starts_with("copy "))
            .count(),
        2
    );
    assert_eq!(
        effects
            .lines()
            .filter(|line| line.starts_with("ssh "))
            .count(),
        2
    );
    let audit = std::fs::read_to_string(state.join("mcp/audit.jsonl")).unwrap();
    assert_eq!(
        audit
            .lines()
            .filter(|line| line.contains("\"tool\":\"deploy\""))
            .count(),
        1,
        "forwarded mutation must settle exactly once: {audit}"
    );
    let context_stderr = std::fs::read_to_string(state.join("context.stderr")).unwrap();
    assert!(
        !context_stderr.contains("serving standalone"),
        "the drill must use the production fleet context, not its fallback: {context_stderr}"
    );

    drop(follower_in);
    let exit = wait_with_timeout(&mut follower, Duration::from_secs(10));
    assert!(exit.success(), "promoted follower exit: {exit:?}");
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
