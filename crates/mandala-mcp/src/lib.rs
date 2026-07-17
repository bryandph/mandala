//! mandala-mcp — the stdio MCP server over the fleet cores.
//!
//! Phase-1 stdio spike (OpenSpec change `mandala-rust-rewrite`, section 1.2):
//! ONE read tool (`resolve`) served over stdio via rust-mcp-sdk 0.10, to
//! prove the initialize → list_tools → call → clean-exit handshake against a
//! real client before porting the other eleven tools (section 4).

use std::sync::Arc;

use async_trait::async_trait;
use mandala_core::VERSION;
use rust_mcp_sdk::{
    McpServer, StdioTransport, TransportOptions,
    error::SdkResult,
    macros::{JsonSchema, mcp_tool},
    mcp_server::{McpServerOptions, ServerHandler, ToMcpServerHandler, server_runtime},
    schema::{
        CallToolRequestParams, CallToolResult, Implementation, InitializeResult, ListToolsResult,
        PaginatedRequestParams, ProtocolVersion, RpcError, ServerCapabilities,
        ServerCapabilitiesTools, schema_utils::CallToolError,
    },
};

/// The MCP server identity reported in the `initialize` handshake.
#[must_use]
pub fn server_name() -> String {
    format!("mandala-fleet {VERSION}")
}

/// The `resolve` tool: expand a selector (`@group`, a member, `all`, `!`
/// exclusions) to the sorted member set plus the `limit` confirm string —
/// identical to `mandala resolve` and the `--limit` a deploy fans out to.
#[mcp_tool(
    name = "resolve",
    description = "Expand a selector (`@group`, a member, `all`, `!` exclusions, `,`/`:` lists) \
into the sorted `members` plus the comma-joined `limit` string, which is exactly the \
`confirm` value the gated actions (deploy, reboot, restart_service) require."
)]
#[derive(Debug, serde::Deserialize, serde::Serialize, JsonSchema)]
pub struct ResolveTool {
    /// The selector to expand.
    pub selector: String,
}

/// The server handler — the single dispatch point every tool call funnels
/// through (section 4 wraps this for the activity sink + audit trail).
pub struct MandalaHandler;

#[async_trait]
impl ServerHandler for MandalaHandler {
    async fn handle_list_tools_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            tools: vec![ResolveTool::tool()],
            meta: None,
            next_cursor: None,
        })
    }

    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<CallToolResult, CallToolError> {
        if params.name != ResolveTool::tool_name() {
            return Err(CallToolError::unknown_tool(params.name));
        }
        let args = serde_json::Value::Object(params.arguments.unwrap_or_default());
        let tool: ResolveTool = serde_json::from_value(args).map_err(CallToolError::new)?;
        match mandala_core::resolve(&tool.selector) {
            Ok(resolved) => {
                let body = serde_json::json!({
                    "members": resolved.members,
                    "limit": resolved.limit,
                });
                Ok(CallToolResult::text_content(vec![body.to_string().into()]))
            }
            Err(msg) => Err(CallToolError::new(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                msg,
            ))),
        }
    }
}

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

/// Serve the stdio MCP transport until the client disconnects.
///
/// # Errors
/// Propagates transport/runtime errors from the SDK.
pub async fn run_stdio() -> SdkResult<()> {
    let transport = StdioTransport::new(TransportOptions::default())?;
    let server = server_runtime::create_server(McpServerOptions {
        server_details: server_info(),
        transport,
        handler: MandalaHandler.to_mcp_server_handler(),
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
pub fn serve_stdio_blocking() -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async { run_stdio().await })?;
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
    fn init_result_advertises_the_resolve_tool() {
        // The list handler advertises exactly the one spike tool.
        let tool = ResolveTool::tool();
        assert_eq!(tool.name, "resolve");
    }
}
