//! Failover behavior at the MCP level (OpenSpec change `mandala-native-tui`,
//! task 3.3 — the fleet-context spec scenarios over the REAL dispatch core):
//!
//! 1. "a deploy survives its leader": the leader launches a registered
//!    command run (a real `CommandRun` child — a harmless shell script, never
//!    ansible/nix), dies abruptly, the orphaned child keeps writing its log,
//!    and after the follower promotes its `deploy_status` attaches the SAME
//!    run via the shared registry.
//! 2. "a promotion race has one winner": two followers lose their leader and
//!    call concurrently — exactly one promotes, both calls succeed, and the
//!    results are byte-identical.
//!
//! (The third 3.3 scenario — CLI reads fall back to local eval with no
//! context — lives in `crates/mandala/tests/cli.rs`, next to the binary it
//! drives.)
//!
//! Leader death is simulated by DROPPING a `RunningHost` (abrupt: no drain,
//! no discovery release — exactly a killed process). One test fn: the
//! process-global `MANDALA_FLEET_STATE` seam cannot race.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mandala_core::inventory::{Inventory, InventoryError};
use mandala_core::registry::{self, Meta, RunLiveness};
use mandala_core::runner::CommandRun;
use mandala_mcp::effects::{
    AdhocError, AdhocOutput, CommandLaunch, DeployLaunch, Effects, EvalFailure, SurveyOutput,
};
use mandala_mcp::{MandalaHandler, handler_dispatch, tool_is_idempotent};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use mandala_context::{
    Acquired, ContextIdentity, ContextSession, FleetContext, HostConfig, HostConfigFactory, acquire,
};

mod common;
use common::{FakeEffects, base_inventory};

/// Effects whose `reboot` launches a REAL registered `CommandRun` around a
/// slow, harmless shell script — the phase-1 command-run machinery, no
/// ansible. Everything else is unexpected.
struct OrphanEffects;

#[async_trait]
impl Effects for OrphanEffects {
    async fn fresh_inventory(&self, _flake: &str) -> Result<Inventory, InventoryError> {
        panic!("unexpected fresh_inventory call (the inventory is preloaded)")
    }
    async fn eval_expected(
        &self,
        _flake: &str,
        _members: &[String],
    ) -> Result<std::collections::BTreeMap<String, String>, EvalFailure> {
        panic!("unexpected eval_expected call")
    }
    async fn repo_rev(&self, _flake: &str) -> Option<String> {
        None
    }
    async fn refresh_snapshots(&self) -> io::Result<SurveyOutput> {
        panic!("unexpected refresh_snapshots call")
    }
    async fn run_adhoc(&self, _argv: Vec<String>) -> Result<AdhocOutput, AdhocError> {
        panic!("unexpected run_adhoc call")
    }
    async fn launch_deploy(
        &self,
        _flake: &str,
        _limit: &str,
        _dry_activate: bool,
        _throttle: i64,
    ) -> io::Result<DeployLaunch> {
        panic!("unexpected launch_deploy call")
    }
    async fn launch_command(
        &self,
        argv: Vec<String>,
        kind: &str,
        _cwd: Option<PathBuf>,
        extra_meta: Meta,
    ) -> io::Result<CommandLaunch> {
        // The real registered runner: registry dir, teed log, recorded pid,
        // background reaper — and, load-bearing here, NO kill_on_drop: the
        // child is never tied to the launching leader's lifetime.
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
        // The "ans-reboot wrapper": a shell child that keeps writing for ~30s
        // (long past the test) so the orphan keeps producing observable output.
        Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "i=0; while [ $i -lt 120 ]; do echo tick $i; i=$((i+1)); sleep 0.25; done".to_string(),
        ])
    }
}

/// A promoted/leader config over the real `MandalaHandler` with `effects`,
/// preloaded with the fixture inventory (no evals anywhere).
fn mcp_factory(effects: impl Effects + 'static) -> HostConfigFactory {
    let effects: Arc<dyn Effects> = Arc::new(effects);
    Arc::new(move || {
        let (events, _) = broadcast::channel::<Value>(64);
        let handler = Arc::new(
            MandalaHandler::with_effects(".", Arc::clone(&effects)).preloaded(base_inventory()),
        );
        HostConfig::new(handler_dispatch(handler), events)
    })
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_failover_scenarios() {
    let scratch = std::env::temp_dir().join(format!(
        "mandala-mcp-failover-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let state = scratch.join("state");
    std::fs::create_dir_all(&state).unwrap();
    unsafe { std::env::set_var("MANDALA_FLEET_STATE", &state) };

    orphaned_run_attaches_after_promotion(&scratch, &state).await;
    promotion_race_at_the_mcp_level(&scratch, &state).await;

    unsafe { std::env::remove_var("MANDALA_FLEET_STATE") };
}

/// Scenario: the leader dies while a registered run it launched is live —
/// the orphaned child keeps writing, and the promoted follower's
/// `deploy_status` attaches the SAME run via the registry.
async fn orphaned_run_attaches_after_promotion(scratch: &std::path::Path, state: &std::path::Path) {
    let flake = scratch.join("flake-orphan");
    std::fs::create_dir_all(&flake).unwrap();
    let identity = ContextIdentity::with_port_range(&flake, 28850, 4).unwrap();

    let leader_config = mcp_factory(OrphanEffects);
    let leader = match acquire(&identity, state, "leader-mcp", move || (leader_config)())
        .await
        .unwrap()
    {
        Acquired::Leader(host) => host,
        Acquired::Follower(_) => panic!("no prior context — must lead"),
    };

    // The follower: no handler, no evaluator, no inventory of its own until
    // (unless) it promotes — its factory builds the promoted handler then.
    let session = ContextSession::acquire(
        identity.clone(),
        state,
        "follower-mcp",
        mcp_factory(FakeEffects::default()),
    )
    .await
    .unwrap();
    assert!(!session.is_leader().await, "the live leader serves");

    // Launch the run THROUGH the context (a forwarded, confirmed reboot).
    let launched = mcp_call(
        &session,
        "reboot",
        json!({"selector": "@k3s", "confirm": "cache,web"}),
    )
    .await;
    assert_eq!(launched["ok"], json!(true), "launch result: {launched}");
    let run_id = launched["run_id"].as_str().expect("run_id").to_string();
    let log = PathBuf::from(launched["log"].as_str().expect("log path"));
    assert_eq!(
        registry::open_run(&run_id)
            .expect("registered run")
            .liveness(),
        RunLiveness::Running,
        "the child must be live before the leader dies"
    );

    // The leader dies abruptly (no drain, no discovery release).
    let size_at_death = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
    drop(leader);

    // The orphaned child keeps writing its stream.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let size = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
        if size > size_at_death {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the orphaned child must keep writing after leader death"
        );
    }

    // The follower's next read fails over: re-race → promote → one retry on
    // its own (fresh) handler — which attaches the SAME run via the registry.
    let snap = mcp_call(&session, "deploy_status", json!({"run_id": run_id})).await;
    assert!(session.is_leader().await, "the follower promoted");
    assert_eq!(
        snap["run_id"],
        json!(run_id),
        "the SAME run, via the registry"
    );
    assert_eq!(snap["kind"], json!("reboot"));
    assert_eq!(
        snap["liveness"],
        json!("running"),
        "the orphan is still live: {snap}"
    );
    assert!(
        snap["output_tail"]
            .as_array()
            .is_some_and(|t| !t.is_empty()),
        "the promoted leader reads the orphan's stream: {snap}"
    );

    // Reap the orphan (don't leave a 30s shell behind the test).
    if let Some(pid) = registry::open_run(&run_id).and_then(|o| o.info.pid()) {
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
    }
    session.shutdown(Duration::from_secs(1)).await;
}

/// Scenario: two followers lose their leader and call concurrently — the
/// bind arbitrates exactly one promotion, and BOTH calls succeed through the
/// one new leader, byte-identically.
async fn promotion_race_at_the_mcp_level(scratch: &std::path::Path, state: &std::path::Path) {
    let flake = scratch.join("flake-race");
    std::fs::create_dir_all(&flake).unwrap();
    let identity = ContextIdentity::with_port_range(&flake, 28855, 4).unwrap();

    let leader_config = mcp_factory(FakeEffects::default());
    let leader = match acquire(&identity, state, "leader-mcp", move || (leader_config)())
        .await
        .unwrap()
    {
        Acquired::Leader(host) => host,
        Acquired::Follower(_) => panic!("no prior context — must lead"),
    };

    let mut followers = Vec::new();
    for i in 0..2 {
        let session = ContextSession::acquire(
            identity.clone(),
            state,
            &format!("follower-{i}"),
            mcp_factory(FakeEffects::default()),
        )
        .await
        .unwrap();
        assert!(!session.is_leader().await);
        followers.push(session);
    }

    drop(leader); // the leader dies with both followers attached

    let mut in_flight = Vec::new();
    for session in &followers {
        let session = session.clone();
        in_flight.push(tokio::spawn(async move {
            session
                .call(
                    "resolve",
                    json!({"selector": "@k3s"}).as_object().cloned().unwrap(),
                    true,
                )
                .await
        }));
    }
    let mut results = Vec::new();
    for task in in_flight {
        let full = task
            .await
            .unwrap()
            .expect("every follower's read succeeds through the new leader");
        results.push(serde_json::to_string(&full).unwrap());
    }
    assert_eq!(
        results[0], results[1],
        "both followers get byte-identical results from the one new leader"
    );
    let payload: Value = serde_json::from_str(&results[0]).unwrap();
    assert_eq!(
        payload["structuredContent"],
        json!({"members": ["cache", "web"], "limit": "cache,web"}),
        "and it is the real tool result"
    );

    let mut leaders = 0;
    for session in &followers {
        if session.is_leader().await {
            leaders += 1;
        }
    }
    assert_eq!(leaders, 1, "the bind arbitrates exactly one promotion");
    for session in followers {
        session.shutdown(Duration::from_secs(1)).await;
    }
}
