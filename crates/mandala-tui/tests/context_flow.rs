//! TUI ↔ fleet-context integration (OpenSpec change `mandala-native-tui`,
//! section 6) through the REAL loop and REAL context endpoints in-process.
//!
//! Two tiers:
//!
//! * **Synthetic-event tests** — the pipeline downstream of the context
//!   subscription (auto-attach with the live-pid guard and no-double-attach,
//!   the drift-landed refresh, the reload swap, the `m` toggle) driven by
//!   injecting `AppEvent::McpActivity` into the loop's channel. No endpoint,
//!   just the registry (a private tmp base) and `sh` children.
//! * **Endpoint tests** — a leader-TUI actually hosting a context (real
//!   `MandalaHandler` dispatch over stub effects, real TCP endpoints, real
//!   `ContextSession` followers): follower calls served + origin-labeled,
//!   the quit-ordering drain proof, observer detach, and the task-6.5
//!   failover drill. Never real ansible/nix — every child is an `sh` stub.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crossterm::event::{Event, KeyCode, KeyEvent};
use futures_util::Stream;
use mandala_context::{
    ContextIdentity, ContextSession, FleetContext, HostConfig, HostConfigFactory, discovery,
};
use mandala_core::inventory::{Inventory, InventoryError};
use mandala_core::registry::{self, Meta, RunLiveness};
use mandala_core::runner::{CommandRun, DeployRun};
use mandala_mcp::effects::{
    AdhocError, AdhocOutput, CommandLaunch, DeployLaunch, Effects, EvalFailure,
};
use mandala_mcp::{MandalaHandler, handler_dispatch, tool_is_idempotent};
use mandala_tui::app::App;
use mandala_tui::context::TuiContext;
use mandala_tui::event::AppEvent;
use mandala_tui::explorer::ExplorerConfig;
use mandala_tui::screen::{ScreenState, TaskState};
use mandala_tui::state::{AppState, ContextRole, LoadedInventory};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::{Value, json};
use tokio::sync::broadcast;

// ---- process-wide state base (set once, before any concurrent read) ---------

fn test_env() -> &'static PathBuf {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    BASE.get_or_init(|| {
        let scratch = std::env::temp_dir().join(format!("mandala-ctxflow-{}", std::process::id()));
        let state = scratch.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let fixture = scratch.join("aggregate.json");
        std::fs::write(&fixture, serde_json::to_vec(&aggregate()).unwrap()).unwrap();
        // SAFETY: set once, under the OnceLock, before any concurrent read in
        // this process (every test calls test_env() first). The aggregate
        // seam makes the LOCAL (no-context / fallback) load path read the
        // fixture instead of evaluating anything.
        unsafe {
            std::env::set_var("MANDALA_FLEET_STATE", &state);
            std::env::set_var("MANDALA_FLEET_RUN_KEEP", "500");
            std::env::set_var("MANDALA_AGGREGATE_FILE", &fixture);
        }
        scratch
    })
}

// ---- fixtures ---------------------------------------------------------------

fn aggregate() -> Value {
    json!({
        "schemaVersion": 1,
        "members": {
            "web": {"platform": "metal"},
            "cache": {"platform": "metal"},
        },
        "groups": {"k3s": ["cache", "web"]},
        "projections": {"deploy": {"nodes": ["cache", "web"]}},
    })
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-06-12T12:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc)
}

fn filled_state() -> AppState {
    let mut state = AppState::new();
    let req = state.request_load().expect("idle state loads");
    let loaded = LoadedInventory {
        inventory: Inventory::from_value(aggregate()).expect("fixture aggregate is valid"),
        rev: Some("aaaaaaaaaaaaaaaa".to_string()),
        cached_rev: None,
        cached: BTreeMap::new(),
    };
    assert!(
        state
            .on_load_finished(req.generation, Ok(loaded), &BTreeMap::new(), now())
            .is_none()
    );
    state
}

fn stub_cfg() -> ExplorerConfig {
    ExplorerConfig {
        survey_argv: vec!["sh".into(), "-c".into(), "sleep 5".into()],
        ..ExplorerConfig::default()
    }
}

// ---- loop driving -----------------------------------------------------------

fn key(code: KeyCode) -> io::Result<Event> {
    Ok(Event::Key(KeyEvent::from(code)))
}

#[allow(clippy::type_complexity)]
fn key_channel() -> (
    tokio::sync::mpsc::UnboundedSender<io::Result<Event>>,
    impl Stream<Item = io::Result<Event>> + Unpin,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let stream = Box::pin(futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|ev| (ev, rx))
    }));
    (tx, stream)
}

/// Drive an app through timed keys on a wide TestBackend (room for the
/// docked panel), closing the stream `close_after` ms after the last key.
/// Returns the app AND the terminal so tests can assert on the buffer.
async fn drive(
    mut app: App,
    script: Vec<(u64, KeyCode)>,
    close_after: u64,
) -> (App, Terminal<TestBackend>) {
    let mut terminal = Terminal::new(TestBackend::new(140, 24)).expect("test terminal");
    let (tx, mut events) = key_channel();
    let driver = tokio::spawn(async move {
        for (delay_ms, code) in script {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            let _ = tx.send(key(code));
        }
        tokio::time::sleep(Duration::from_millis(close_after)).await;
        drop(tx);
    });
    app.run(&mut terminal, &mut events)
        .await
        .expect("loop runs");
    driver.await.expect("driver task");
    (app, terminal)
}

// ---- registry fixtures ------------------------------------------------------

fn make_run(meta_pairs: &[(&str, Value)]) -> (String, PathBuf) {
    test_env();
    let (run_id, path) = registry::new_run_dir().unwrap();
    let mut meta = Meta::new();
    meta.insert("run_id".into(), Value::from(run_id.clone()));
    for (k, v) in meta_pairs {
        meta.insert((*k).to_string(), v.clone());
    }
    registry::write_meta(&path, &meta).unwrap();
    (run_id, path)
}

fn settle(tool: &str, seq: u64, origin: &str, extra: Value) -> Value {
    let mut event = json!({
        "tool": tool, "args": {}, "status": "ok", "detail": null,
        "seq": seq, "elapsed": 0.5, "origin": origin,
    });
    for (k, v) in extra.as_object().into_iter().flatten() {
        event[k] = v.clone();
    }
    event
}

// =============================================================================
// Synthetic-event tier: the subscription's downstream pipeline
// =============================================================================

/// Flag-independent auto-attach: a client's settled reboot attaches the
/// read-only log observer for the EXACT run the result summary names — only
/// once, and only while the recorded pid is alive.
#[tokio::test]
async fn settle_auto_attaches_reboot_with_live_pid_guard_and_no_double_attach() {
    test_env();
    let (run_id, _path) = make_run(&[
        ("kind", Value::from("reboot")),
        ("limit", Value::from("web,db")),
        ("pid", Value::from(i64::from(std::process::id()))),
    ]);
    let app = App::new(filled_state(), stub_cfg());
    app.sender()
        .send(AppEvent::McpActivity {
            event: settle(
                "reboot",
                1,
                "mcp-9",
                json!({"result": {"ok": true, "run_id": run_id}}),
            ),
        })
        .await
        .unwrap();
    let (app, _terminal) = drive(app, vec![], 400).await;
    let Some(ScreenState::AttachedLog(attached)) = &app.state.screen else {
        panic!("attached-log screen not up: {:?}", app.state.screen);
    };
    assert_eq!(attached.run_id, run_id);
    assert_eq!(attached.title, "reboot web,db");
    assert!(
        attached.after_mutation,
        "client runs refresh drift on close"
    );
    assert!(app.state.attached_runs.contains(&run_id));

    // Replay the same settle after a detach (the carried pure state in a
    // fresh loop — `quit` is sticky on a driven App): the run never
    // re-attaches.
    let mut detached = app.state.clone();
    detached.screen = None;
    let app = App::new(detached, stub_cfg());
    app.sender()
        .send(AppEvent::McpActivity {
            event: settle(
                "reboot",
                2,
                "mcp-9",
                json!({"result": {"ok": true, "run_id": run_id}}),
            ),
        })
        .await
        .unwrap();
    let (app, _terminal) = drive(app, vec![], 300).await;
    assert!(
        app.state.screen.is_none(),
        "an already-attached run must not re-attach: {:?}",
        app.state.screen
    );

    // A dead recorded pid attaches nothing (a refused call launches nothing
    // but still settles ok — only a live run is the one just fired).
    let (dead_id, _) = make_run(&[
        ("kind", Value::from("reboot")),
        ("pid", Value::from(999_999_999_i64)),
    ]);
    let app = App::new(filled_state(), stub_cfg());
    app.sender()
        .send(AppEvent::McpActivity {
            event: settle(
                "reboot",
                3,
                "mcp-9",
                json!({"result": {"ok": true, "run_id": dead_id}}),
            ),
        })
        .await
        .unwrap();
    let (app, _terminal) = drive(app, vec![], 300).await;
    assert!(app.state.screen.is_none(), "dead pid must not attach");
    assert!(!app.state.attached_runs.contains(&dead_id));
}

/// A client's deploy settle attaches the live per-host deploy view (attached
/// mode: esc detaches, nothing re-launches); an already-open screen is never
/// clobbered by an attach.
#[tokio::test]
async fn settle_auto_attaches_deploy_screen_and_never_clobbers_an_open_screen() {
    test_env();
    let (run_id, path) = make_run(&[
        ("limit", Value::from("web")),
        ("dry_activate", Value::from(false)),
        ("pid", Value::from(i64::from(std::process::id()))),
    ]);
    std::fs::write(
        path.join("web.jsonl"),
        "{\"v\":1,\"ts\":0,\"host\":\"web\",\"plugin\":\"deploy\",\"event\":\"milestone\",\"milestone\":\"copy\"}\n",
    )
    .unwrap();
    let app = App::new(filled_state(), stub_cfg());
    app.sender()
        .send(AppEvent::McpActivity {
            event: settle(
                "deploy",
                1,
                "agent-7",
                json!({"result": {"ok": true, "run_id": run_id}}),
            ),
        })
        .await
        .unwrap();
    let (app, _terminal) = drive(app, vec![], 500).await;
    let Some(ScreenState::Deploy(view)) = &app.state.screen else {
        panic!("deploy screen not up: {:?}", app.state.screen);
    };
    assert!(view.attached, "a client run is observed, never owned");
    assert!(view.after_mutation);
    assert_eq!(view.limit, "web");
    assert_eq!(view.hosts[0].name, "web");

    // Single-screen-slot adaptation: a screen already up is never clobbered.
    let (other_id, _) = make_run(&[
        ("kind", Value::from("reboot")),
        ("pid", Value::from(i64::from(std::process::id()))),
    ]);
    let mut busy = App::new(filled_state(), stub_cfg());
    busy.state.screen = Some(ScreenState::Task(TaskState::new("ping web", 1, false)));
    busy.sender()
        .send(AppEvent::McpActivity {
            event: settle(
                "reboot",
                2,
                "agent-7",
                json!({"result": {"ok": true, "run_id": other_id}}),
            ),
        })
        .await
        .unwrap();
    let (busy, _terminal) = drive(busy, vec![], 300).await;
    assert!(
        matches!(busy.state.screen, Some(ScreenState::Task(_))),
        "an open screen survives a client settle"
    );
    assert!(!busy.state.attached_runs.contains(&other_id));
}

/// A settled client `drift(refresh/do_eval)` re-reads the shared state like
/// an operator S-landing; a settled client `reload` swaps the inventory by
/// re-running the load path (local fallback here — no context joined).
#[tokio::test]
async fn drift_and_reload_settles_update_the_views() {
    test_env();
    let app = App::new(filled_state(), stub_cfg());
    app.sender()
        .send(AppEvent::McpActivity {
            event: settle("drift", 1, "mcp-9", json!({"args": {"refresh": true}})),
        })
        .await
        .unwrap();
    let (app, _terminal) = drive(app, vec![], 300).await;
    assert_eq!(app.state.status, "drift refreshed (mcp)");
    assert_eq!(
        app.state.drift_rows.len(),
        2,
        "the drift table repainted over the deploy nodes"
    );

    // reload: the queued load re-reads the contract (the aggregate-file seam
    // stands in for the leader's already-evaluated swap) and repaints.
    let app = App::new(filled_state(), stub_cfg());
    assert_eq!(app.state.generation, 0);
    app.sender()
        .send(AppEvent::McpActivity {
            event: settle("reload", 2, "mcp-9", json!({})),
        })
        .await
        .unwrap();
    let (app, _terminal) = drive(app, vec![], 600).await;
    assert_eq!(app.state.generation, 1, "the reload rebound the inventory");
    assert!(app.state.inventory.is_some(), "the follow-up load landed");
    assert!(
        app.state.status.starts_with("2 members"),
        "the fill repainted from the fresh contract: {:?}",
        app.state.status
    );
}

/// The `m` toggle exists only under `--debug-mcp` (the binding is inert and
/// unhinted without the flag).
#[tokio::test]
async fn m_toggles_the_panel_only_under_debug_mcp() {
    test_env();
    let mut state = filled_state();
    state.debug_mcp = true;
    let app = App::new(state, stub_cfg());
    let (app, _terminal) = drive(app, vec![(20, KeyCode::Char('m'))], 100).await;
    assert!(!app.state.mcp_panel, "m hides the panel");

    let app = App::new(filled_state(), stub_cfg());
    let (app, _terminal) = drive(app, vec![(20, KeyCode::Char('m'))], 100).await;
    assert!(app.state.mcp_panel, "without the flag the binding is inert");
}

/// The debug pane renders the settled log (origin-labeled) and the pending
/// strip (collapsing when empty); without the flag no monitoring surface
/// exists at all.
#[tokio::test]
async fn debug_panel_renders_and_is_absent_without_the_flag() {
    test_env();
    let mut state = filled_state();
    state.debug_mcp = true;
    state.context_role = Some(ContextRole::Leader);
    let _ = state.on_mcp_activity(&settle(
        "resolve",
        1,
        "agent-1",
        json!({"args": {"selector": "@k3s"}}),
    ));
    let _ = state.on_mcp_activity(&json!({
        "tool": "drift", "args": {"do_eval": true}, "status": "start",
        "detail": null, "seq": 2, "origin": "agent-1",
    }));
    let mut terminal = Terminal::new(TestBackend::new(140, 24)).unwrap();
    terminal
        .draw(|frame| mandala_tui::render::render(&state, frame))
        .unwrap();
    let grid = terminal.backend().to_string();
    assert!(grid.contains("mcp activity"), "panel present:\n{grid}");
    assert!(grid.contains("▸ resolve"), "settled line rendered:\n{grid}");
    assert!(grid.contains("⟨agent-1⟩"), "origin-labeled:\n{grid}");
    assert!(grid.contains("[ok · 0.5s]"), "python label format:\n{grid}");
    assert!(grid.contains("selector='@k3s'"), "args rendered:\n{grid}");
    assert!(
        grid.contains("⠋ drift"),
        "pending strip spins the in-flight call:\n{grid}"
    );
    assert!(grid.contains("mcp drift"), "status-bar mcp job:\n{grid}");
    assert!(grid.contains("ctx leader"), "role indicator:\n{grid}");

    // The strip collapses when the in-flight call settles.
    let _ = state.on_mcp_activity(&settle(
        "drift",
        2,
        "agent-1",
        json!({"args": {"do_eval": true}}),
    ));
    terminal
        .draw(|frame| mandala_tui::render::render(&state, frame))
        .unwrap();
    let grid = terminal.backend().to_string();
    assert!(
        !grid.contains("⠋ drift"),
        "pending strip collapsed:\n{grid}"
    );

    // Without the flag: the same state renders NO monitoring surface (the
    // subscription bookkeeping is still there — it drives auto-attach).
    state.debug_mcp = false;
    terminal
        .draw(|frame| mandala_tui::render::render(&state, frame))
        .unwrap();
    let grid = terminal.backend().to_string();
    assert!(!grid.contains("mcp activity"), "no panel:\n{grid}");
    assert!(!grid.contains("▸ resolve"), "no log lines:\n{grid}");
    assert!(!grid.contains("m mcp panel"), "no footer hint:\n{grid}");
}

// =============================================================================
// Endpoint tier: leader-TUI hosting a REAL context
// =============================================================================

/// Stub effects behind the REAL `MandalaHandler`: reads serve the preloaded
/// fixture; `reload` re-reads it (optionally slowly — the drain proof);
/// reboot launches a REAL registered `CommandRun` around a ticking `sh`
/// child (the phase-1 machinery, no ansible anywhere).
struct StubEffects {
    inventory_delay: Duration,
}

#[async_trait]
impl Effects for StubEffects {
    async fn fresh_inventory(&self, _flake: &str) -> Result<Inventory, InventoryError> {
        tokio::time::sleep(self.inventory_delay).await;
        Ok(Inventory::from_value(aggregate()).expect("fixture aggregate"))
    }
    async fn eval_expected(
        &self,
        _flake: &str,
        members: &[String],
    ) -> Result<BTreeMap<String, String>, EvalFailure> {
        Ok(members
            .iter()
            .map(|m| (m.clone(), format!("/nix/store/expected-{m}")))
            .collect())
    }
    async fn repo_rev(&self, _flake: &str) -> Option<String> {
        None
    }
    async fn refresh_snapshots(&self) -> io::Result<i32> {
        Ok(0)
    }
    async fn run_adhoc(&self, _argv: Vec<String>) -> Result<AdhocOutput, AdhocError> {
        panic!("unexpected run_adhoc call")
    }
    async fn launch_deploy(&self, _limit: &str, _dry_activate: bool) -> io::Result<DeployLaunch> {
        panic!("unexpected launch_deploy call")
    }
    async fn launch_command(
        &self,
        argv: Vec<String>,
        kind: &str,
        _cwd: Option<PathBuf>,
        extra_meta: Meta,
    ) -> io::Result<CommandLaunch> {
        // The real registered runner — registry dir, teed log, recorded pid,
        // NO kill_on_drop: the child survives its launching leader.
        let mut run = CommandRun::new(argv, kind);
        run.extra_meta = extra_meta;
        run.start().await?;
        Ok(CommandLaunch {
            run_id: run.run_id.clone().unwrap_or_default(),
            log: run.log_path().unwrap_or_default(),
            launched: run.launched(),
        })
    }
    fn reboot_argv(&self, _target: &str, _serial: &str, _drain: bool) -> Option<Vec<String>> {
        Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "i=0; while [ $i -lt 120 ]; do echo tick $i; i=$((i+1)); sleep 0.25; done".to_string(),
        ])
    }
}

fn stub_factory(inventory_delay: Duration) -> HostConfigFactory {
    Arc::new(move || {
        let (events, _) = broadcast::channel::<Value>(64);
        let sink_events = events.clone();
        let handler = Arc::new(
            MandalaHandler::with_effects(".", Arc::new(StubEffects { inventory_delay }))
                .preloaded(Inventory::from_value(aggregate()).expect("fixture aggregate"))
                .with_sink(Arc::new(move |event: &Value| {
                    let _ = sink_events.send(event.clone());
                })),
        );
        HostConfig::new(handler_dispatch(handler), events)
    })
}

async fn acquire_tui(
    scratch: &std::path::Path,
    name: &str,
    base_port: u16,
    delay: Duration,
) -> (ContextIdentity, ContextSession) {
    let flake_dir = scratch.join(format!("flake-{base_port}"));
    std::fs::create_dir_all(&flake_dir).unwrap();
    let identity = ContextIdentity::with_port_range(&flake_dir, base_port, 4).unwrap();
    let session = ContextSession::acquire(
        identity.clone(),
        test_env().join("state"),
        name,
        stub_factory(delay),
    )
    .await
    .unwrap();
    (identity, session)
}

/// One MCP call through the seam, unwrapped to its structured result.
async fn mcp_call(ctx: &dyn FleetContext, tool: &str, args: Value) -> Value {
    let map = args.as_object().cloned().unwrap_or_default();
    let full = ctx
        .call(tool, map, tool_is_idempotent(tool))
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e}"));
    full.get("structuredContent")
        .cloned()
        .unwrap_or_else(|| json!({}))
}

/// A leader-TUI serves a follower's call, and the call renders in the TUI's
/// activity machinery origin-labeled (fleet-mcp "a tool call appears in the
/// debug activity view").
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leader_tui_serves_follower_calls_origin_labeled() {
    let scratch = test_env().clone();
    let (identity, session) = acquire_tui(&scratch, "tui-1", 28880, Duration::from_millis(0)).await;
    assert!(
        session.is_leader().await,
        "no prior context — the TUI leads"
    );

    let mut state = filled_state();
    state.debug_mcp = true;
    let mut app = App::new(state, stub_cfg());
    app.adopt_context(TuiContext {
        session: session.clone(),
        client_name: "tui-1".to_string(),
        leader: true,
    });

    let follower = ContextSession::acquire(
        identity.clone(),
        test_env().join("state"),
        "agent-1",
        stub_factory(Duration::from_millis(0)),
    )
    .await
    .unwrap();
    assert!(!follower.is_leader().await, "the TUI's endpoint answered");

    let call = {
        let follower = follower.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            mcp_call(&follower, "resolve", json!({"selector": "@k3s"})).await
        })
    };
    let (app, terminal) = drive(app, vec![], 900).await;
    let result = call.await.unwrap();
    assert_eq!(
        result,
        json!({"members": ["cache", "web"], "limit": "cache,web"}),
        "the leader-TUI executed the follower's call"
    );
    let entry = app
        .state
        .mcp_log
        .iter()
        .find(|e| e.tool == "resolve")
        .expect("the follower's call rendered in the TUI's activity log");
    assert_eq!(entry.origin.as_deref(), Some("agent-1"));
    assert!(entry.ok);
    let grid = terminal.backend().to_string();
    assert!(grid.contains("▸ resolve"), "pane line:\n{grid}");
    assert!(grid.contains("⟨agent-1⟩"), "origin label:\n{grid}");

    follower.shutdown(Duration::from_millis(200)).await;
    let mut app = app;
    app.shutdown_context(Duration::from_secs(1)).await;
}

/// Quit ordering (task 6.4, the drain proof at the TUI level): a follower's
/// in-flight MUTATION completes during the leader-TUI's orderly shutdown —
/// never a structured failover — and the discovery claim is released.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leader_tui_quit_drains_an_in_flight_follower_call() {
    let scratch = test_env().clone();
    // reload takes 500ms at the leader: in flight across the quit.
    let (identity, session) =
        acquire_tui(&scratch, "tui-1", 28885, Duration::from_millis(500)).await;
    assert!(session.is_leader().await);

    let mut app = App::new(filled_state(), stub_cfg());
    app.adopt_context(TuiContext {
        session: session.clone(),
        client_name: "tui-1".to_string(),
        leader: true,
    });

    let follower = ContextSession::acquire(
        identity.clone(),
        test_env().join("state"),
        "agent-1",
        stub_factory(Duration::from_millis(0)),
    )
    .await
    .unwrap();
    let call = {
        let follower = follower.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            // A mutation: no retry masks a failed drain.
            follower.call("reload", serde_json::Map::new(), false).await
        })
    };

    // Quit while the forwarded call is in flight, then run the orderly
    // shutdown exactly as `run_explorer` does (before terminal restore).
    let (mut app, _terminal) = drive(app, vec![(250, KeyCode::Char('q'))], 50).await;
    app.shutdown_context(Duration::from_secs(3)).await;

    let outcome = call.await.unwrap();
    assert!(
        outcome.is_ok(),
        "the in-flight call must drain through the quit: {outcome:?}"
    );
    assert!(
        discovery::read(&test_env().join("state"), identity.key()).is_none(),
        "an orderly leader-TUI quit releases its discovery claim"
    );
    follower.shutdown(Duration::from_millis(200)).await;
}

/// Observer quit = clean detach: the leader keeps serving other clients.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn observer_tui_quit_is_a_clean_detach() {
    let scratch = test_env().clone();
    let (identity, leader) = acquire_tui(&scratch, "mcp-0", 28890, Duration::from_millis(0)).await;
    assert!(leader.is_leader().await);

    let tui = ContextSession::acquire(
        identity.clone(),
        test_env().join("state"),
        "tui-2",
        stub_factory(Duration::from_millis(0)),
    )
    .await
    .unwrap();
    let mut app = App::new(filled_state(), stub_cfg());
    app.adopt_context(TuiContext {
        session: tui,
        client_name: "tui-2".to_string(),
        leader: false,
    });
    assert_eq!(app.state.context_role, Some(ContextRole::Observer));
    let (mut app, _terminal) = drive(app, vec![(50, KeyCode::Char('q'))], 50).await;
    app.shutdown_context(Duration::from_secs(1)).await;

    // The leader is untouched: a fresh client still gets served.
    let client = ContextSession::acquire(
        identity.clone(),
        test_env().join("state"),
        "agent-2",
        stub_factory(Duration::from_millis(0)),
    )
    .await
    .unwrap();
    assert!(!client.is_leader().await, "the leader survived the detach");
    let result = mcp_call(&client, "members", json!({})).await;
    assert!(result.get("web").is_some());
    client.shutdown(Duration::from_millis(200)).await;
    leader.shutdown(Duration::from_millis(200)).await;
}

/// Task 6.5 — the failover drill: a leader-TUI (headless, hosting a real
/// context) auto-attaches a follower's registered reboot run, then quits;
/// the follower promotes and the run stays attachable via the registry; a
/// restarted observer-TUI attaches to the NEW leader and sees its activity.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failover_drill_run_survives_leader_tui_death() {
    let scratch = test_env().clone();
    let (identity, session) = acquire_tui(&scratch, "tui-1", 28895, Duration::from_millis(0)).await;
    assert!(
        session.is_leader().await,
        "the TUI claims the fresh context"
    );

    let mut state = filled_state();
    state.debug_mcp = true;
    let mut app = App::new(state, stub_cfg());
    app.adopt_context(TuiContext {
        session: session.clone(),
        client_name: "tui-1".to_string(),
        leader: true,
    });

    // An mcp-shaped follower launches a confirmed reboot through the
    // leader-TUI — a REAL registered CommandRun `sh` child.
    let follower = ContextSession::acquire(
        identity.clone(),
        test_env().join("state"),
        "mcp-1",
        stub_factory(Duration::from_millis(0)),
    )
    .await
    .unwrap();
    let launch = {
        let follower = follower.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            mcp_call(
                &follower,
                "reboot",
                json!({"selector": "web", "confirm": "web"}),
            )
            .await
        })
    };
    let (app, _terminal) = drive(app, vec![], 1200).await;
    let launched = launch.await.unwrap();
    assert_eq!(launched["ok"], json!(true), "launch result: {launched}");
    let run_id = launched["run_id"].as_str().expect("run_id").to_string();

    // The client-triggered run rendered like a human one: auto-attached.
    let Some(ScreenState::AttachedLog(attached)) = &app.state.screen else {
        panic!("client reboot did not auto-attach: {:?}", app.state.screen);
    };
    assert_eq!(attached.run_id, run_id);
    let entry = app
        .state
        .mcp_log
        .iter()
        .find(|e| e.tool == "reboot")
        .expect("the reboot settle rendered");
    assert_eq!(entry.origin.as_deref(), Some("mcp-1"));

    // The leader-TUI dies (orderly quit — the run's child is NOT its child).
    let mut app = app;
    app.shutdown_context(Duration::from_secs(1)).await;
    drop(app);

    // The follower's next read fails over → it promotes → the SAME run
    // attaches via the shared registry, still live.
    let snap = mcp_call(&follower, "deploy_status", json!({"run_id": run_id})).await;
    assert!(follower.is_leader().await, "the follower promoted");
    assert_eq!(
        snap["run_id"],
        json!(run_id),
        "the SAME run, via the registry"
    );
    assert_eq!(
        snap["liveness"],
        json!("running"),
        "the orphan lives: {snap}"
    );

    // A restarted observer-TUI joins the NEW leader and sees its activity.
    let restarted = ContextSession::acquire(
        identity.clone(),
        test_env().join("state"),
        "tui-2",
        stub_factory(Duration::from_millis(0)),
    )
    .await
    .unwrap();
    assert!(!restarted.is_leader().await, "the promoted follower leads");
    let mut state = filled_state();
    state.debug_mcp = true;
    let mut app = App::new(state, stub_cfg());
    app.adopt_context(TuiContext {
        session: restarted,
        client_name: "tui-2".to_string(),
        leader: false,
    });
    let activity = {
        let follower = follower.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            mcp_call(&follower, "resolve", json!({"selector": "@k3s"})).await
        })
    };
    let (mut app, _terminal) = drive(app, vec![], 800).await;
    activity.await.unwrap();
    let entry = app
        .state
        .mcp_log
        .iter()
        .find(|e| e.tool == "resolve")
        .expect("the restarted TUI observes the new leader's activity");
    assert!(
        entry.origin.is_none(),
        "the new leader's own calls carry no origin and still render"
    );
    app.shutdown_context(Duration::from_millis(200)).await;

    // Reap the orphan (don't leave a 30s shell behind the test).
    if let Some(pid) = registry::open_run(&run_id).and_then(|o| o.info.pid()) {
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
    }
    let mut obs = registry::open_run(&run_id).expect("run still registered");
    let _ = obs.liveness() == RunLiveness::Running; // touch to silence unused
    follower.shutdown(Duration::from_millis(200)).await;
}

/// A DeployRun attach through a real registry run keeps working end-to-end
/// (regression guard for the section-5 machinery the auto-attach reuses).
#[tokio::test]
async fn deploy_run_attach_seam_still_holds() {
    test_env();
    let (run_id, _path) = make_run(&[
        ("limit", Value::from("web")),
        ("dry_activate", Value::from(false)),
        ("pid", Value::from(i64::from(std::process::id()))),
    ]);
    assert!(DeployRun::attach(&run_id).is_some());
}
