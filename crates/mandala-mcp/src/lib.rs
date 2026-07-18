//! mandala-mcp — the stdio MCP server over the fleet cores.
//!
//! The full 12-tool surface (OpenSpec change `mandala-rust-rewrite`, section
//! 4), at result-shape parity with the Python FastMCP server (golden fixtures
//! under `cli/tests/fixtures/mcp/` are the oracle — see
//! `tests/parity.rs`). Reads: `members`, `groups`, `resolve`, `ping`,
//! `host_eval`, `drift`, `reload`. Actions: `deploy_status`, `build`,
//! `deploy`, `restart_service`, `reboot` — the gated three refuse without a
//! `confirm` equal to the resolved `--limit`.
//!
//! rust-mcp-sdk has no middleware, so the Python `ActivityMiddleware` is a
//! dispatch wrapper around `handle_call_tool_request`
//! ([`MandalaHandler::call_tool`]): start/settle events with a shared `seq`,
//! `elapsed`, and a result summary; mutating settles always append to
//! `state_dir()/mcp/audit.jsonl` — the audit trail exists even headless.
//!
//! Since OpenSpec change `mandala-native-tui` (section 3), every `mandala
//! mcp` instance participates in the per-checkout fleet execution context
//! ([`crate::context`]): the first instance leads (hosting the coordination
//! endpoint and the one warm evaluator), later ones serve their stdio
//! conversation locally but forward tool execution to the leader — one
//! inventory, one activity stream, one audit trail per checkout.

pub mod activity;
pub mod context;
pub mod effects;
pub mod server;
pub mod tools;

use std::time::Duration;

use mandala_context::{ContextIdentity, ContextSession};
use rust_mcp_sdk::error::SdkResult;
use rust_mcp_sdk::mcp_server::{McpServerOptions, ToMcpServerHandler, server_runtime};
use rust_mcp_sdk::schema::{
    Implementation, InitializeResult, ProtocolVersion, ServerCapabilities, ServerCapabilitiesTools,
};
use rust_mcp_sdk::{McpServer as _, StdioTransport, TransportOptions};

pub use context::{ContextHandler, handler_dispatch, host_config_factory, tool_is_idempotent};
pub use server::{MandalaHandler, server_name};

use mandala_core::VERSION;

/// How long a leader drains in-flight forwarded calls on stdin EOF before
/// abandoning them (their clients see the connection close as a structured
/// failover and re-race). Bounded so closing a harness never hangs behind a
/// long `deploy_status` wait.
const CONTEXT_SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// Build the `InitializeResult` (server identity + capabilities) advertised
/// in the handshake.
fn server_info() -> InitializeResult {
    InitializeResult {
        server_info: Implementation {
            name: "mandala-fleet".into(),
            version: VERSION.into(),
            title: Some("mandala fleet".into()),
            description: Some("AI-operator porcelain over the mandala fleet cores".into()),
            icons: vec![],
            website_url: None,
        },
        capabilities: ServerCapabilities {
            tools: Some(ServerCapabilitiesTools { list_changed: None }),
            ..Default::default()
        },
        protocol_version: ProtocolVersion::V2025_06_18.into(),
        instructions: None,
        meta: None,
    }
}

/// Serve one stdio MCP conversation with `handler` until the client
/// disconnects (stdin EOF).
async fn serve_over_stdio(handler: impl ToMcpServerHandler) -> SdkResult<()> {
    let transport = StdioTransport::new(TransportOptions::default())?;
    let server = server_runtime::create_server(McpServerOptions {
        server_details: server_info(),
        transport,
        handler: handler.to_mcp_server_handler(),
        task_store: None,
        client_task_store: None,
        message_observer: None,
    });
    server.start().await
}

/// Serve the stdio MCP transport inside the checkout's fleet execution
/// context: join the live leader as a forwarding follower, or claim the
/// context and lead (building the server never evaluates the fleet — the
/// first read does). On stdin EOF a leader performs the orderly context
/// shutdown (refuse-new → drain → close → guarded discovery release) BEFORE
/// returning, so followers detect the death and re-race; a follower simply
/// disconnects. When the checkout cannot host a context at all (non-path
/// flake ref, exhausted port range) the server degrades loudly to the
/// standalone context-free shape.
///
/// # Errors
/// Propagates transport/runtime errors from the SDK.
pub async fn run_stdio(flake: &str) -> SdkResult<()> {
    let Some(session) = join_context(flake).await else {
        return run_stdio_standalone(flake).await;
    };
    let result = serve_over_stdio(ContextHandler::new(session.clone())).await;
    // The stdio conversation is over: release leadership (or just hang up).
    // Runs launched here stay attachable — their children are never tied to
    // this process's lifetime (the orphan-run semantics are load-bearing).
    session.shutdown(CONTEXT_SHUTDOWN_GRACE).await;
    result
}

/// The context-free stdio server — the pre-context shape, kept as the
/// fallback when no context can exist for `flake`.
///
/// # Errors
/// Propagates transport/runtime errors from the SDK.
pub async fn run_stdio_standalone(flake: &str) -> SdkResult<()> {
    serve_over_stdio(MandalaHandler::new(flake)).await
}

/// Join (or claim) the fleet execution context for `flake`; `None` — with a
/// stderr notice — when the checkout cannot host one.
async fn join_context(flake: &str) -> Option<ContextSession> {
    let identity = match ContextIdentity::for_flake(flake) {
        Ok(identity) => identity,
        Err(e) => {
            eprintln!("mandala mcp: no context for flake '{flake}' ({e}); serving standalone");
            return None;
        }
    };
    let state_dir = mandala_core::drift::state_dir();
    let client_name = format!("mcp-{}", std::process::id());
    match ContextSession::acquire(
        identity,
        state_dir,
        client_name,
        context::host_config_factory(flake),
    )
    .await
    {
        Ok(session) => Some(session),
        Err(e) => {
            eprintln!("mandala mcp: context unavailable ({e}); serving standalone");
            None
        }
    }
}

/// Blocking entry point for the `mandala mcp` subcommand: owns the tokio
/// runtime so the binary's `main` stays synchronous.
///
/// # Errors
/// Propagates any runtime-build or transport error.
pub fn serve_stdio_blocking(flake: &str) -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async { run_stdio(flake).await })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_name_reports_the_version() {
        assert!(server_name().starts_with("mandala-fleet"));
    }

    #[test]
    fn init_result_advertises_tool_capability() {
        let info = server_info();
        assert!(info.capabilities.tools.is_some());
        assert_eq!(info.server_info.name, "mandala-fleet");
    }
}
