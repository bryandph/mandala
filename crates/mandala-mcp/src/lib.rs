//! mandala-mcp — the stdio MCP server over the fleet cores.
//!
//! The legacy tool surface (OpenSpec change `mandala-rust-rewrite`, section
//! 4), originally ported at result-shape parity with the retired Python
//! FastMCP server; the surviving parity gate is leader-served vs
//! follower-proxied byte-identity (`tests/parity_proxy.rs`). Reads:
//! `members`, `groups`, `resolve`, `ping`,
//! `host_eval`, `drift`, `reload`. Actions: `deploy_status`, `build`,
//! `deploy`, `restart_service`, `reboot` — the gated three refuse without a
//! `confirm` equal to the resolved `--limit`. Opt-in `campaign_*` tools add
//! a durable immutable target and generation-checked lifecycle alongside it.
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

use mandala_context::{AcquireError, AcquireOptions, ContextIdentity, ContextSession};
use rust_mcp_sdk::error::SdkResult;
use rust_mcp_sdk::mcp_server::{McpServerOptions, ToMcpServerHandler, server_runtime};
use rust_mcp_sdk::schema::{
    Implementation, InitializeResult, ServerCapabilities, ServerCapabilitiesTools,
};
use rust_mcp_sdk::{McpServer as _, StdioTransport, TransportOptions};

pub use context::{
    ContextHandler, handler_dispatch, host_config_factory, quiet_host_config_factory,
    tool_is_idempotent,
};
pub use server::{MandalaHandler, server_name};

/// The protocol versions this server can negotiate, re-exported so callers
/// (and the stdio integration tests) can assert the handshake surface without
/// taking their own `rust-mcp-sdk` dependency.
pub use rust_mcp_sdk::schema::ProtocolVersion;

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
        // Track the SDK's newest schema rather than pinning a date: a client
        // that negotiates a version NEWER than the one advertised here gets
        // its `initialize` dropped with no reply and no stderr, which the
        // client can only report as a connect timeout (Claude Code moved to
        // 2025-11-25 and hung for 30s against a pinned 2025-06-18).
        protocol_version: ProtocolVersion::latest().into(),
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
/// disconnects. Non-path flake refs retain context-free standalone behavior.
/// A canonical checkout's acquisition failure instead initializes an error-only
/// MCP server: every tool reports the explicit error, with no effectful fallback.
///
/// # Errors
/// Propagates transport/runtime errors from the SDK.
pub async fn run_stdio(flake: &str) -> SdkResult<()> {
    run_stdio_with_options(flake, AcquireOptions::default()).await
}

/// Stdio frontend with explicit context options for isolated embedding/tests.
///
/// # Errors
/// SDK transport/runtime errors. Acquisition failures are explicit tool errors
/// after successful MCP initialization, never an effectful fallback.
pub async fn run_stdio_with_options(flake: &str, options: AcquireOptions) -> SdkResult<()> {
    let session = match join_context(flake, options).await {
        Ok(Some(session)) => session,
        Ok(None) => return run_stdio_standalone(flake).await,
        Err(error) => {
            // Acquisition already completed mandatory failed-host teardown.
            // Keep protocol initialization responsive, but never degrade a
            // canonical checkout into an unguarded effectful standalone core.
            eprintln!("mandala mcp: context acquisition failed ({error}); all tools unavailable");
            return serve_over_stdio(context::UnavailableContextHandler::new(&error)).await;
        }
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
/// stderr notice — for a non-path flake; canonical acquisition errors remain errors.
async fn join_context(
    flake: &str,
    options: AcquireOptions,
) -> Result<Option<ContextSession>, AcquireError> {
    let identity = match ContextIdentity::for_flake(flake) {
        Ok(identity) => identity,
        Err(e) => {
            eprintln!("mandala mcp: no context for flake '{flake}' ({e}); serving standalone");
            return Ok(None);
        }
    };
    let state_dir = mandala_core::drift::state_dir();
    let client_name = format!("mcp-{}", std::process::id());
    match ContextSession::acquire_with_options(
        identity,
        state_dir,
        client_name,
        context::host_config_factory(flake),
        options,
    )
    .await
    {
        Ok(session) => Ok(Some(session)),
        Err(error) => Err(error),
    }
}

/// Blocking entry point for the `mandala mcp` subcommand: owns the tokio
/// runtime so the binary's `main` stays synchronous.
///
/// Bounded teardown (the TUI 7.4 quit-hang finding, same class here): after
/// the conversation ends — the orderly context shutdown included — any eval
/// still blocked in the worker roundtrip's synchronous stdout read on the
/// blocking pool would hold `Runtime` drop (and process exit) hostage until
/// the eval completed. Kill the worker child(ren) and latch respawns off,
/// then hard-bound the runtime shutdown. Deploy/ansible children are never
/// touched (orphan-run semantics stay load-bearing).
///
/// # Errors
/// Propagates any runtime-build or transport error.
pub fn serve_stdio_blocking(flake: &str) -> Result<(), Box<dyn std::error::Error>> {
    serve_stdio_blocking_with_root(flake, None)
}

/// Blocking frontend with an explicit, pre-created isolated coordination root.
///
/// # Errors
/// See [`serve_stdio_blocking`].
pub fn serve_stdio_blocking_with_root(
    flake: &str,
    coordination_root: Option<std::path::PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = rt.block_on(async {
        run_stdio_with_options(
            flake,
            AcquireOptions {
                coordination_root,
                ..AcquireOptions::default()
            },
        )
        .await
    });
    mandala_core::eval::shutdown_workers();
    rt.shutdown_timeout(std::time::Duration::from_secs(2));
    result?;
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
