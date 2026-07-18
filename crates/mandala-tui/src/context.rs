//! The TUI's fleet-context participation (OpenSpec change
//! `mandala-native-tui`, section 6 — capability `fleet-context`).
//!
//! The TUI joins the per-checkout execution context SYMMETRICALLY, exactly
//! like `mandala mcp`: with no leader present it claims the context itself
//! (later `mandala mcp` instances proxy through this process); with a leader
//! present it attaches as an observer. Either way it holds ONE
//! [`ContextSession`] — the same promotion state machine — so a dead leader
//! makes the TUI re-race like any client, and if it wins it starts hosting
//! seamlessly.
//!
//! Dependency direction (decision of record): mandala-tui depends on
//! mandala-mcp and consumes [`mandala_mcp::quiet_host_config_factory`] /
//! [`mandala_mcp::tool_is_idempotent`] directly — mandala-mcp never depends
//! on mandala-tui, so there is no cycle and no shared factory crate is
//! needed. The TUI's factory is the QUIET variant: when the TUI hosts the
//! leader, the leader's children (eval worker stderr, survey output) must
//! never write through the alternate screen.
//!
//! One warm worker (decision of record): under a context, EVERY eval-class
//! explorer read — the aggregate load, `r`'s contract reload, `S`'s
//! expected-toplevel eval — routes through [`FleetContext::call`] to the
//! leader's handler and its one evaluator (which IS this process's evaluator
//! when the TUI leads). The TUI spawns its own local evaluator only in the
//! no-context fallback ([`crate::explorer`]'s jobs, unchanged). Drift
//! snapshots, the expected cache, and the run registry are FILES in the
//! shared state dir — those stay direct reads.

use std::collections::BTreeMap;
use std::time::Duration;

use mandala_context::{CallError, ContextIdentity, ContextSession, FleetContext};
use mandala_core::drift;
use mandala_core::inventory::{Inventory, SUPPORTED_SCHEMA_VERSION};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::event::AppEvent;
use crate::explorer::ExplorerConfig;
use crate::state::{LoadRequest, LoadedInventory};

/// How long a leader-TUI drains in-flight forwarded calls on quit before
/// abandoning them (their clients see the close as a structured failover and
/// re-race) — the same bound as `mandala mcp`'s stdin-EOF shutdown.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// How often the role watcher re-checks leadership (an event is sent only on
/// an actual change, so the loop is not woken by the poll itself).
const ROLE_POLL: Duration = Duration::from_millis(500);

/// The TUI's joined context: the session plus the identity its calls are
/// labeled with in other participants' activity streams.
pub struct TuiContext {
    pub session: ContextSession,
    /// The hello identity (`tui-<pid>`) — also the self-filter key: the
    /// TUI's own calls are already represented by the explorer's job flags
    /// and must not double-render as activity.
    pub client_name: String,
    /// Whether the session held leadership at join time (the initial role
    /// indicator; later changes arrive via the role watcher).
    pub leader: bool,
}

/// Join (or claim) the fleet execution context for `flake` — the same
/// join-or-claim path `mandala mcp` uses, with the TUI's quiet leader
/// factory. `None` (with a stderr notice — the terminal is not yet in raw
/// mode) when the checkout cannot host a context; the explorer then runs on
/// its local evaluator exactly as before.
pub async fn join_context(flake: &str) -> Option<TuiContext> {
    let identity = match ContextIdentity::for_flake(flake) {
        Ok(identity) => identity,
        Err(e) => {
            eprintln!("mandala tui: no context for flake '{flake}' ({e}); evaluating locally");
            return None;
        }
    };
    let state_dir = drift::state_dir();
    let client_name = format!("tui-{}", std::process::id());
    match ContextSession::acquire(
        identity,
        state_dir,
        client_name.clone(),
        mandala_mcp::quiet_host_config_factory(flake),
    )
    .await
    {
        Ok(session) => {
            let leader = session.is_leader().await;
            Some(TuiContext {
                session,
                client_name,
                leader,
            })
        }
        Err(e) => {
            eprintln!("mandala tui: context unavailable ({e}); evaluating locally");
            None
        }
    }
}

/// Pump the context's activity stream into the loop's internal channel —
/// ONE pipeline whether the TUI leads (its own broadcast) or observes (the
/// leader's stream); [`ContextSession::subscribe`] resumes across promotions
/// transparently. The subscription is flag-independent: settle events drive
/// run auto-attach, the drift-landed refresh, and remote-reload swaps even
/// with no `--debug-mcp` surface.
pub fn spawn_activity_pump(session: ContextSession, tx: mpsc::Sender<AppEvent>) {
    tokio::spawn(async move {
        let Ok(mut events) = session.subscribe().await else {
            return;
        };
        while let Some(event) = events.recv().await {
            if tx.send(AppEvent::McpActivity { event }).await.is_err() {
                return;
            }
        }
    });
}

/// Watch the session's role, reporting CHANGES only (the subtle status
/// indicator's feed): an observer that wins a post-leader-death re-race
/// reports `leader = true`; a leader never demotes. Exits when the app's
/// receiver is gone.
pub fn spawn_role_watch(session: ContextSession, tx: mpsc::Sender<AppEvent>, mut leader: bool) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(ROLE_POLL).await;
            let now = session.is_leader().await;
            if now != leader {
                leader = now;
                if tx.send(AppEvent::McpRoleChanged { leader }).await.is_err() {
                    return;
                }
            }
        }
    });
}

/// Why a context-routed read did not produce a result.
enum ReadFailure {
    /// The executing side answered with a tool-level error — surfaced like a
    /// local eval failure, never a fallback trigger (the call RAN).
    Tool(String),
    /// The context is gone (failover chain exhausted / re-acquisition
    /// failed / session closed) — fall back to the local evaluator.
    Gone,
}

/// One tool call through the seam, unwrapped to its `structuredContent`.
/// Idempotency comes from the one declared table (`tool_is_idempotent`), so
/// reads retry once across a promotion exactly like `mandala mcp`'s.
async fn call_structured(
    session: &ContextSession,
    tool: &str,
    args: Value,
) -> Result<Value, ReadFailure> {
    let map = args.as_object().cloned().unwrap_or_default();
    match session
        .call(tool, map, mandala_mcp::tool_is_idempotent(tool))
        .await
    {
        Ok(value) => Ok(value
            .get("structuredContent")
            .cloned()
            .unwrap_or(Value::Null)),
        Err(CallError::Tool(msg)) => Err(ReadFailure::Tool(msg)),
        Err(_) => Err(ReadFailure::Gone),
    }
}

/// `r` under a context: the contract refresh IS the `reload` tool — it
/// re-roots the LEADER's eval worker and swaps the shared inventory, so
/// every participant's next read sees the fresh contract (and their TUIs
/// swap via the reload settle event). A failover during the (non-retryable)
/// reload is adopted as success: the promoted leader starts cold, so the
/// fetch that follows evaluates fresh by construction.
async fn reload_over_context(session: &ContextSession) -> Result<(), ReadFailure> {
    match session.call("reload", serde_json::Map::new(), false).await {
        Ok(_) | Err(CallError::Failover { .. }) => Ok(()),
        Err(CallError::Tool(msg)) => Err(ReadFailure::Tool(msg)),
        Err(_) => Err(ReadFailure::Gone),
    }
}

/// The aggregate load over the context: the leader's warm evaluator serves
/// `members(full)` + `groups`, and `drift {}`'s entries name the deploy
/// nodes (`drift::compare` iterates exactly the deploy projection) — enough
/// to reconstruct a faithful aggregate for [`Inventory::from_value`] without
/// touching the frozen tool surface. The drift-cache inspection beside it
/// (repo rev, `.expected.json`) stays local: files and git, not eval.
async fn load_over_context(
    session: &ContextSession,
    flake: &str,
    fresh: bool,
) -> Result<LoadedInventory, ReadFailure> {
    if fresh {
        reload_over_context(session).await?;
    }
    let members = call_structured(session, "members", json!({"full": true})).await?;
    let groups = call_structured(session, "groups", json!({})).await?;
    let drift_view = call_structured(session, "drift", json!({})).await?;
    let nodes: Vec<Value> = drift_view
        .get("entries")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| e.get("host").cloned())
                .collect()
        })
        .unwrap_or_default();
    let aggregate = json!({
        "schemaVersion": SUPPORTED_SCHEMA_VERSION,
        "members": members,
        "groups": groups,
        "projections": {"deploy": {"nodes": nodes}},
    });
    let inventory =
        Inventory::from_value(aggregate).map_err(|e| ReadFailure::Tool(e.to_string()))?;
    let flake = flake.to_string();
    let (rev, cached_rev, cached) = tokio::task::spawn_blocking(move || {
        let rev = drift::repo_rev(&flake);
        let (cached_rev, cached) = drift::load_expected(&drift::state_dir());
        (rev, cached_rev, cached)
    })
    .await
    .unwrap_or((None, None, BTreeMap::new()));
    Ok(LoadedInventory {
        inventory,
        rev,
        cached_rev,
        cached,
    })
}

/// Spawn the context-routed aggregate load; falls back to the local
/// evaluator job when the context is gone mid-session (a promotion that made
/// US the leader is not "gone" — the session's retry already served the read
/// locally).
pub fn spawn_context_load(
    tx: mpsc::Sender<AppEvent>,
    session: ContextSession,
    cfg: ExplorerConfig,
    req: LoadRequest,
) {
    tokio::spawn(async move {
        match load_over_context(&session, &cfg.flake, req.fresh).await {
            Ok(loaded) => {
                let _ = tx
                    .send(AppEvent::LoadFinished {
                        generation: req.generation,
                        result: Ok(loaded),
                    })
                    .await;
            }
            Err(ReadFailure::Tool(msg)) => {
                let _ = tx
                    .send(AppEvent::LoadFinished {
                        generation: req.generation,
                        result: Err(msg),
                    })
                    .await;
            }
            Err(ReadFailure::Gone) => crate::explorer::spawn_load(tx, cfg, req),
        }
    });
}

/// The `S` expected-toplevel eval over the context: `drift {do_eval: true}`
/// evaluates at the leader AND writes the shared `.expected.json` cache
/// (same state dir); the expected map is read back off the entries. An
/// `eval_error` in the result surfaces exactly like a local eval failure.
async fn eval_expected_over_context(
    session: &ContextSession,
) -> Result<Result<(Option<String>, BTreeMap<String, String>), String>, ReadFailure> {
    let view = call_structured(session, "drift", json!({"do_eval": true})).await?;
    if let Some(err) = view.get("eval_error") {
        let msg = err
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("expected-toplevel eval failed");
        return Ok(Err(format!("eval failed: {msg}")));
    }
    let rev = view.get("rev").and_then(Value::as_str).map(str::to_string);
    let mut expected = BTreeMap::new();
    for entry in view
        .get("entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let (Some(host), Some(top)) = (
            entry.get("host").and_then(Value::as_str),
            entry.get("expected").and_then(Value::as_str),
        ) {
            expected.insert(host.to_string(), top.to_string());
        }
    }
    Ok(Ok((rev, expected)))
}

/// Spawn the context-routed expected eval; same fallback discipline as the
/// load (`inventory` rides along only for the local fallback's node
/// resolution).
pub fn spawn_context_eval_expected(
    tx: mpsc::Sender<AppEvent>,
    session: ContextSession,
    cfg: ExplorerConfig,
    inventory: Option<Inventory>,
) {
    tokio::spawn(async move {
        match eval_expected_over_context(&session).await {
            Ok(result) => {
                let _ = tx.send(AppEvent::DriftEvalFinished { result }).await;
            }
            Err(ReadFailure::Tool(msg)) => {
                let _ = tx
                    .send(AppEvent::DriftEvalFinished {
                        result: Err(format!("eval failed: {msg}")),
                    })
                    .await;
            }
            Err(ReadFailure::Gone) => crate::explorer::spawn_eval_expected(tx, cfg, inventory),
        }
    });
}
