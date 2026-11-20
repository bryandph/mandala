//! Acquisition failures still initialize MCP, but every tool fails explicitly.
#[path = "../../mandala-context/tests/common/mod.rs"]
mod context_fixture;
use mandala_context::{ContextIdentity, election};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn error_server_process() {
    let Some(root) = std::env::var_os("MANDALA_ERROR_SERVER_FIXTURE") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let state = root.join("state");
    let options = context_fixture::options(&state);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(mandala_mcp::run_stdio_with_options(
        root.join("flake").to_str().unwrap(),
        options,
    ))
    .unwrap();
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
fn send(input: &mut impl Write, message: Value) {
    writeln!(input, "{message}").unwrap();
    input.flush().unwrap();
}
fn response(output: &mpsc::Receiver<Value>, id: u64) -> Value {
    loop {
        let message = output
            .recv_timeout(Duration::from_secs(30))
            .expect("MCP initializes within its handshake budget");
        if message["id"] == id {
            return message;
        }
    }
}

#[test]
fn io_and_busy_failures_initialize_then_refuse_every_tool_without_effects() {
    context_fixture::refusal_preflight();
    for kind in ["io", "coordination_busy"] {
        let root = std::env::temp_dir().join(format!(
            "mcp-acquire-error-{kind}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("flake")).unwrap();
        let state = root.join("state");
        let options = context_fixture::options(&state);
        let identity = ContextIdentity::for_flake(root.join("flake")).unwrap();
        let held = if kind == "coordination_busy" {
            Some(election::try_acquire(&identity, &options).unwrap().unwrap())
        } else {
            std::fs::create_dir_all(state.join("mcp")).unwrap();
            std::fs::write(state.join("mcp/contexts"), "fixture publication failure").unwrap();
            None
        };
        let mut server = Server(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "error_server_process", "--nocapture"])
                .env("MANDALA_ERROR_SERVER_FIXTURE", &root)
                .env("MANDALA_FLEET_STATE", &state)
                .env("HOME", root.join("home"))
                .env("XDG_STATE_HOME", root.join("xdg"))
                .env("XDG_RUNTIME_DIR", root.join("runtime"))
                // Invalid effects configuration would fail differently if any tool
                // reached the ordinary handler instead of the error-only server.
                .env(
                    "MANDALA_AGGREGATE_FILE",
                    root.join("must-not-read-aggregate"),
                )
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut input = server.0.stdin.take().unwrap();
        let stdout = server.0.stdout.take().unwrap();
        let (tx, output) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(value) = serde_json::from_str::<Value>(&line)
                    && tx.send(value).is_err()
                {
                    break;
                }
            }
        });
        send(
            &mut input,
            json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"protocolVersion":"2025-11-25", "capabilities":{}, "clientInfo":{"name":"isolated-error-test","version":"1"}}}),
        );
        let initialized = response(&output, 1);
        assert!(
            initialized["result"]["serverInfo"].is_object(),
            "initialize succeeds: {initialized}"
        );
        send(
            &mut input,
            json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
        );
        if kind == "io" {
            assert!(
                election::try_acquire(&identity, &options)
                    .unwrap()
                    .is_some(),
                "failed host released its process lease before initialize"
            );
            let listener = std::net::TcpListener::bind((
                std::net::Ipv4Addr::LOCALHOST,
                identity.derived_port(),
            ))
            .expect("failed host listener closed before initialize");
            drop(listener);
        }
        send(
            &mut input,
            json!({"jsonrpc":"2.0", "id":2, "method":"tools/list", "params":{}}),
        );
        let listed = response(&output, 2);
        let tools = listed["result"]["tools"].as_array().unwrap();
        assert!(!tools.is_empty());
        for (i, tool) in tools.iter().enumerate() {
            let id = i as u64 + 3;
            send(
                &mut input,
                json!({"jsonrpc":"2.0", "id":id, "method":"tools/call", "params":{"name":tool["name"],"arguments":{}}}),
            );
            let result = response(&output, id);
            assert_eq!(
                result["result"]["isError"], true,
                "tool {}: {result}",
                tool["name"]
            );
            assert_eq!(
                result["result"]["structuredContent"]["error"],
                "context_acquisition_failed"
            );
            assert_eq!(result["result"]["structuredContent"]["kind"], kind);
            assert_eq!(result["result"]["structuredContent"]["tool"], tool["name"]);
        }
        assert!(
            !state.join("runs").exists(),
            "no mutation dispatch or run allocation"
        );
        assert!(
            !state.join("mcp/audit.jsonl").exists(),
            "no ordinary handler dispatched tools"
        );
        drop(input);
        assert!(server.0.wait().unwrap().success());
        reader.join().unwrap();
        let mut stderr = String::new();
        server
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(
            stderr.contains("context acquisition failed")
                && stderr.contains("all tools unavailable"),
            "loud acquisition diagnostic: {stderr}"
        );
        drop(held);
        std::fs::remove_dir_all(root).unwrap();
    }
}
