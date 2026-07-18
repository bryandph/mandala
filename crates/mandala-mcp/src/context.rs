//! `mandala mcp` inside the fleet execution context (OpenSpec change
//! `mandala-native-tui`, task 3.1 — capabilities `fleet-context` +
//! `fleet-mcp`).
//!
//! Every stdio instance serves the MCP *conversation* (initialize, the static
//! tools/list) locally, but tool *execution* goes through the one
//! [`FleetContext`] seam — a [`ContextSession`]:
//!
//! - **Leader**: the session holds the bind; its [`HostConfigFactory`] built
//!   a fresh [`MandalaHandler`] (real effects — the evaluator spawns its
//!   worker lazily on the first eval, the inventory stays `None` until the
//!   first read) whose dispatch serves BOTH this instance's own stdio calls
//!   (origin `None`) and forwarded calls from followers (origin = their
//!   hello identity). One dispatch, one activity stream, one audit trail.
//! - **Follower**: no handler, no evaluator, no inventory — every call
//!   forwards to the leader over the coordination endpoint.
//! - **Promotion**: when the leader dies the session re-races the bind; the
//!   winner's factory runs again, building a fresh handler (fresh eval
//!   worker, lazy first eval — the accepted post-failover cold start).
//!
//! Retry discipline is declared here, per tool ([`tool_is_idempotent`]):
//! reads are retried once after a promotion; mutations — including `build`
//! and `reload`, non-idempotent by effect even though ungated — surface the
//! structured failover error instead (the run registry, not a retry, says
//! what actually launched).
//!
//! `reload` executes AT the leader like every call: it re-roots the leader's
//! eval worker and swaps the leader's shared inventory, so every instance's
//! subsequent calls (all executed at the leader) see the fresh contract.

use std::sync::Arc;

use async_trait::async_trait;
use mandala_context::{
    CallError, ContextSession, Dispatch, FleetContext, HostConfig, HostConfigFactory,
};
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::mcp_server::ServerHandler;
use rust_mcp_sdk::schema::schema_utils::CallToolError;
use rust_mcp_sdk::schema::{
    CallToolRequestParams, CallToolResult, ListToolsResult, PaginatedRequestParams, RpcError,
};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::server::MandalaHandler;
use crate::tools::all_tools;

/// Whether a tool call is retry-safe across a leader failover — the
/// declaration [`ContextSession::call`]'s retry discipline enforces.
///
/// Reads are idempotent (`members`, `groups`, `resolve`, `ping`, `host_eval`,
/// `drift`, `deploy_status`); everything else is a mutation and is NEVER
/// retried — `deploy`/`reboot`/`restart_service` for the obvious reason, and
/// `build`/`reload` because they are non-idempotent by effect (a launched nix
/// build, a swapped inventory) even though they carry no confirm gate.
#[must_use]
pub fn tool_is_idempotent(name: &str) -> bool {
    matches!(
        name,
        "members" | "groups" | "resolve" | "ping" | "host_eval" | "drift" | "deploy_status"
    )
}

/// Wrap a [`MandalaHandler`] as the context [`Dispatch`]: origin threads
/// straight into [`MandalaHandler::call_tool_from`] (stamping activity and
/// audit), and the `CallToolResult` rides the wire as its serialized JSON.
#[must_use]
pub fn handler_dispatch(handler: Arc<MandalaHandler>) -> Dispatch {
    Arc::new(move |origin: Option<String>, tool: String, args| {
        let handler = Arc::clone(&handler);
        Box::pin(async move {
            match handler.call_tool_from(origin.as_deref(), &tool, args).await {
                Ok(result) => serde_json::to_value(result).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        })
    })
}

/// The leader-side config factory for `flake` — [`ContextSession`]'s
/// promotion hook. Each invocation (the initial claim, and every later
/// promotion) builds a FRESH [`MandalaHandler`] over production effects: a
/// fresh evaluator whose worker spawns lazily on the first eval, and a lazy
/// inventory (`None` until the first read — the server's existing laziness).
/// The handler's activity sink publishes into the endpoint's broadcast, so
/// subscribers see every call this leader executes, origin-labeled.
#[must_use]
pub fn host_config_factory(flake: &str) -> HostConfigFactory {
    let flake = flake.to_string();
    Arc::new(move || {
        let (events, _) = broadcast::channel::<Value>(256);
        let sink_events = events.clone();
        let handler = Arc::new(MandalaHandler::new(&flake).with_sink(Arc::new(
            move |event: &Value| {
                let _ = sink_events.send(event.clone());
            },
        )));
        HostConfig::new(handler_dispatch(handler), events)
    })
}

/// The stdio [`ServerHandler`] of a context-joined instance: the MCP
/// conversation is served locally (the tool list is static), execution goes
/// through the session — leader-local dispatch or follower forwarding, the
/// same code path either way.
pub struct ContextHandler {
    session: ContextSession,
}

impl ContextHandler {
    /// A handler over an acquired session (leader or follower — invisible
    /// here, by design).
    #[must_use]
    pub fn new(session: ContextSession) -> Self {
        Self { session }
    }
}

#[async_trait]
impl ServerHandler for ContextHandler {
    async fn handle_list_tools_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            tools: all_tools(),
            meta: None,
            next_cursor: None,
        })
    }

    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<CallToolResult, CallToolError> {
        let idempotent = tool_is_idempotent(&params.name);
        match self
            .session
            .call(
                &params.name,
                params.arguments.unwrap_or_default(),
                idempotent,
            )
            .await
        {
            // The executing side's CallToolResult, deserialized back off the
            // seam — byte-identical to a directly served call (the parity
            // gates in tests/parity*.rs).
            Ok(value) => serde_json::from_value(value).map_err(CallToolError::new),
            // The tool itself failed at the leader: the message round-trips
            // verbatim (CallToolError displays exactly its message).
            Err(CallError::Tool(msg)) => Err(CallToolError::from_message(msg)),
            // Failover / re-acquisition / closed: the structured error text
            // (execution state unknown; retried/promoted) surfaces to the
            // client instead of a hang — fleet-context requirement.
            Err(other) => Err(CallToolError::from_message(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotency_table_matches_the_declared_tiers() {
        for read in [
            "members",
            "groups",
            "resolve",
            "ping",
            "host_eval",
            "drift",
            "deploy_status",
        ] {
            assert!(tool_is_idempotent(read), "{read} is a retry-safe read");
        }
        // build/reload are ungated but non-idempotent by effect.
        for mutation in ["deploy", "reboot", "restart_service", "build", "reload"] {
            assert!(
                !tool_is_idempotent(mutation),
                "{mutation} must never auto-retry"
            );
        }
        assert!(
            !tool_is_idempotent("no-such-tool"),
            "unknowns are not retried"
        );
    }
}
