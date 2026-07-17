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

pub mod activity;
pub mod effects;
pub mod server;
pub mod tools;

use rust_mcp_sdk::error::SdkResult;
use rust_mcp_sdk::mcp_server::{McpServerOptions, ToMcpServerHandler, server_runtime};
use rust_mcp_sdk::schema::{
    Implementation, InitializeResult, ProtocolVersion, ServerCapabilities, ServerCapabilitiesTools,
};
use rust_mcp_sdk::{McpServer as _, StdioTransport, TransportOptions};

pub use server::{MandalaHandler, server_name};

use mandala_core::VERSION;

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

/// Serve the stdio MCP transport over `flake`'s inventory until the client
/// disconnects. Building the server never evaluates the fleet — the first
/// read does.
///
/// # Errors
/// Propagates transport/runtime errors from the SDK.
pub async fn run_stdio(flake: &str) -> SdkResult<()> {
    let transport = StdioTransport::new(TransportOptions::default())?;
    let server = server_runtime::create_server(McpServerOptions {
        server_details: server_info(),
        transport,
        handler: MandalaHandler::new(flake).to_mcp_server_handler(),
        task_store: None,
        client_task_store: None,
        message_observer: None,
    });
    server.start().await
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
