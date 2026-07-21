//! The server: shared state, the activity dispatch wrapper, and the 12 tool
//! implementations.
//!
//! Pure presentation: every tool delegates to the inventory/drift/registry
//! cores (via [`crate::effects::Effects`] for anything that spawns or
//! evaluates), the same ones the CLI reads, so a selector here resolves to
//! exactly what `mandala resolve` and `ansible -l` project. The slow inputs
//! (a host's `toplevel` eval, the drift survey, the expected-toplevel eval)
//! run only when a tool argument explicitly asks for them, mirroring the
//! CLI's opt-ins.
//!
//! Building the handler never evaluates the fleet — the first read triggers
//! the one `nix eval .#mandala`, gated by `schemaVersion` in the inventory
//! core. The `reload` tool evaluates a fresh aggregate and swaps the shared
//! `RwLock<Option<Inventory>>` (the Rust replacement for the Python
//! getter/slot dance — see the design's decisions).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mandala_core::inventory::Inventory;
use mandala_core::registry::{self, Meta, ObservedRun, RunLiveness};
use mandala_core::runner::{COMMAND_LOG, HostState, ansible_dir};
use mandala_core::{VERSION, drift};
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::mcp_server::ServerHandler;
use rust_mcp_sdk::schema::schema_utils::CallToolError;
use rust_mcp_sdk::schema::{
    CallToolRequestParams, CallToolResult, ListToolsResult, PaginatedRequestParams, RpcError,
    TextContent,
};
use serde_json::{Value, json};

use crate::activity::{ActivitySink, audit_event, result_summary};
use crate::effects::{AdhocError, AdhocOutput, Effects, EvalFailure, RealEffects};
use crate::tools::{
    BuildTool, DeployStatusTool, DeployTool, DriftTool, GroupsTool, HostEvalTool, MembersTool,
    PingTool, RebootTool, ReloadTool, ResolveTool, RestartServiceTool, all_tools,
};

/// The MCP server identity reported in the `initialize` handshake.
#[must_use]
pub fn server_name() -> String {
    format!("mandala-fleet {VERSION}")
}

/// Blocking waits stay under typical MCP client timeouts.
const MAX_WAIT_SECONDS: i64 = 570;

/// Keep the CLI/TUI native-engine fan-out default at the MCP boundary.
const DEPLOY_THROTTLE: i64 = 4;

/// Lines of a command run's teed log surfaced in its snapshot.
const OUTPUT_TAIL: usize = 120;

/// systemd unit names an MCP client may restart: a plain name (dots, @, :)
/// only — anything shell-ish or path-ish is refused before ansible sees it.
/// (The Python `_UNIT_RE`: `^[A-Za-z0-9][A-Za-z0-9@:._-]*$`.)
fn valid_unit(unit: &str) -> bool {
    let mut bytes = unit.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'@' | b':' | b'.' | b'_' | b'-'))
}

/// The reboot playbook's `serial`: a batch count or a percentage. Anything
/// else is refused — ansible parses `-e "a=1 b=2"` as MULTIPLE extra-vars,
/// so an unvalidated string here would be an extra-vars injection point.
/// (The Python `_SERIAL_RE`: `^[0-9]+%?$`.)
fn valid_serial(serial: &str) -> bool {
    let digits = serial.strip_suffix('%').unwrap_or(serial);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// A tool-level error (the FastMCP `ToolError` equivalent): surfaced to the
/// client as an `isError` result whose text is exactly this message.
fn tool_error(msg: impl Into<String>) -> CallToolError {
    CallToolError::from_message(msg.into())
}

/// The Python `errors.failure()` shape: `ok=False` plus diagnostic context.
fn failure_value(summary: &str, f: &EvalFailure) -> Value {
    json!({
        "ok": false,
        "error": summary,
        "command": f.command,
        "exit_code": f.exit_code,
        "output": f.output,
    })
}

/// The shared server state behind every tool call.
struct ServerState {
    flake: String,
    /// The served inventory: `None` until the lazy first read, swapped whole
    /// by the `reload` tool. Read via a clone (aggregates are small).
    inventory: RwLock<Option<Inventory>>,
    /// Whether `reload` may swap the inventory (the stdio server always can;
    /// a getter-only host — the Python TUI wiring — cannot).
    reloadable: bool,
    effects: Arc<dyn Effects>,
    sink: Option<ActivitySink>,
    seq: AtomicU64,
}

/// The server handler — the single dispatch point every tool call funnels
/// through. The activity wrapper ([`MandalaHandler::call_tool`]) emits
/// start/settle events around each dispatch and appends mutating settles to
/// the audit trail, reproducing the Python `ActivityMiddleware` exactly.
pub struct MandalaHandler {
    state: Arc<ServerState>,
}

impl MandalaHandler {
    /// A handler over the production effects (evaluator, runners, ansible).
    #[must_use]
    pub fn new(flake: impl Into<String>) -> Self {
        Self::with_effects(flake, Arc::new(RealEffects::new()))
    }

    /// A handler over injected effects (the parity tests' seam).
    #[must_use]
    pub fn with_effects(flake: impl Into<String>, effects: Arc<dyn Effects>) -> Self {
        MandalaHandler {
            state: Arc::new(ServerState {
                flake: flake.into(),
                inventory: RwLock::new(None),
                reloadable: true,
                effects,
                sink: None,
                seq: AtomicU64::new(1),
            }),
        }
    }

    /// Pre-populate the served inventory (tests inject an aggregate here, so
    /// no tool call triggers an eval).
    #[must_use]
    pub fn preloaded(self, inventory: Inventory) -> Self {
        *self
            .state
            .inventory
            .write()
            .expect("inventory lock poisoned") = Some(inventory);
        self
    }

    /// Whether the `reload` tool may swap the inventory (default true).
    #[must_use]
    pub fn reloadable(mut self, yes: bool) -> Self {
        Arc::get_mut(&mut self.state)
            .expect("reloadable() must be called before the handler is shared")
            .reloadable = yes;
        self
    }

    /// Attach an activity sink receiving every start/settle event (the
    /// phase-2 TUI's pane feed). The audit trail is written regardless.
    #[must_use]
    pub fn with_sink(mut self, sink: ActivitySink) -> Self {
        Arc::get_mut(&mut self.state)
            .expect("with_sink() must be called before the handler is shared")
            .sink = Some(sink);
        self
    }

    /// Emit one activity event: audit first (best-effort, transport-
    /// independent), then the optional sink.
    fn emit(&self, event: &Value) {
        audit_event(event);
        if let Some(sink) = &self.state.sink {
            sink(event);
        }
    }

    /// The activity dispatch wrapper: `start` at entry, `ok`/`error` at
    /// settle (sharing a `seq`, carrying `elapsed` + a result summary), then
    /// the result. This is the one funnel every tool call passes through —
    /// the audit trail exists even headless. Local calls carry no origin;
    /// context-forwarded calls go through [`MandalaHandler::call_tool_from`].
    ///
    /// # Errors
    /// Tool-level failures (unknown tool, invalid arguments, `ToolError`-tier
    /// refusals like an unknown member) — surfaced to the client as an
    /// `isError` result carrying the message.
    pub async fn call_tool(
        &self,
        name: &str,
        args: serde_json::Map<String, Value>,
    ) -> Result<CallToolResult, CallToolError> {
        self.call_tool_from(None, name, args).await
    }

    /// [`MandalaHandler::call_tool`] with an explicit ORIGIN: the hello
    /// `client` identity of the context connection a forwarded call arrived
    /// on. When present it is stamped on both activity events of the pair —
    /// and thereby on the audit entry of a mutating settle (an ADDED audit
    /// field: forward-compatible per fleet-state-formats; every existing
    /// field is untouched). Leader-local calls pass `None` and carry no
    /// origin at all.
    ///
    /// # Errors
    /// Same as [`MandalaHandler::call_tool`].
    pub async fn call_tool_from(
        &self,
        origin: Option<&str>,
        name: &str,
        args: serde_json::Map<String, Value>,
    ) -> Result<CallToolResult, CallToolError> {
        let seq = self.state.seq.fetch_add(1, Ordering::Relaxed);
        let args_value = Value::Object(args.clone());
        let started = Instant::now();
        self.emit(&with_origin(
            json!({
                "tool": name, "args": args_value, "status": "start",
                "detail": null, "seq": seq,
            }),
            origin,
        ));
        match self.dispatch(name, args).await {
            Ok(value) => {
                self.emit(&with_origin(
                    json!({
                        "tool": name, "args": args_value, "status": "ok",
                        "detail": null, "seq": seq,
                        "elapsed": round3(started.elapsed().as_secs_f64()),
                        "result": result_summary(&value),
                    }),
                    origin,
                ));
                Ok(to_call_result(value))
            }
            Err(err) => {
                self.emit(&with_origin(
                    json!({
                        "tool": name, "args": args_value, "status": "error",
                        "detail": err.to_string(), "seq": seq,
                        "elapsed": round3(started.elapsed().as_secs_f64()),
                    }),
                    origin,
                ));
                Err(err)
            }
        }
    }

    /// Route one call to its tool implementation.
    async fn dispatch(
        &self,
        name: &str,
        args: serde_json::Map<String, Value>,
    ) -> Result<Value, CallToolError> {
        // Parse in a standalone statement per arm: a `?` temporary held
        // across the `.await` would make the future `!Send` (CallToolError
        // boxes a non-Send error).
        let value = Value::Object(args);
        match name {
            n if n == MembersTool::tool_name() => {
                let t: MembersTool = parse(value)?;
                self.tool_members(t).await
            }
            n if n == GroupsTool::tool_name() => self.tool_groups().await,
            n if n == ResolveTool::tool_name() => {
                let t: ResolveTool = parse(value)?;
                self.tool_resolve(t).await
            }
            n if n == PingTool::tool_name() => {
                let t: PingTool = parse(value)?;
                self.tool_ping(t).await
            }
            n if n == HostEvalTool::tool_name() => {
                let t: HostEvalTool = parse(value)?;
                self.tool_host_eval(t).await
            }
            n if n == DriftTool::tool_name() => {
                let t: DriftTool = parse(value)?;
                self.tool_drift(t).await
            }
            n if n == ReloadTool::tool_name() => self.tool_reload().await,
            n if n == DeployStatusTool::tool_name() => {
                let t: DeployStatusTool = parse(value)?;
                self.tool_deploy_status(t).await
            }
            n if n == BuildTool::tool_name() => {
                let t: BuildTool = parse(value)?;
                self.tool_build(t).await
            }
            n if n == DeployTool::tool_name() => {
                let t: DeployTool = parse(value)?;
                self.tool_deploy(t).await
            }
            n if n == RestartServiceTool::tool_name() => {
                let t: RestartServiceTool = parse(value)?;
                self.tool_restart_service(t).await
            }
            n if n == RebootTool::tool_name() => {
                let t: RebootTool = parse(value)?;
                self.tool_reboot(t).await
            }
            other => Err(CallToolError::unknown_tool(other.to_string())),
        }
    }

    /// The served inventory, lazily evaluated on the first read (the one slow
    /// `nix eval .#mandala`) and cached until `reload` swaps it.
    async fn inventory(&self) -> Result<Inventory, CallToolError> {
        if let Ok(guard) = self.state.inventory.read()
            && let Some(inv) = guard.as_ref()
        {
            return Ok(inv.clone());
        }
        let fresh = self
            .state
            .effects
            .fresh_inventory(&self.state.flake)
            .await
            .map_err(|e| tool_error(e.to_string()))?;
        if let Ok(mut guard) = self.state.inventory.write() {
            *guard = Some(fresh.clone());
        }
        Ok(fresh)
    }

    // ---- read tier ----------------------------------------------------------

    async fn tool_members(&self, t: MembersTool) -> Result<Value, CallToolError> {
        let inv = self.inventory().await?;
        if t.full.unwrap_or(false) {
            return serde_json::to_value(inv.members()).map_err(CallToolError::new);
        }
        let compact: serde_json::Map<String, Value> = inv
            .members()
            .iter()
            .map(|(name, member)| (name.clone(), Value::Object(member.compact())))
            .collect();
        Ok(Value::Object(compact))
    }

    async fn tool_groups(&self) -> Result<Value, CallToolError> {
        let inv = self.inventory().await?;
        serde_json::to_value(inv.groups()).map_err(CallToolError::new)
    }

    async fn tool_resolve(&self, t: ResolveTool) -> Result<Value, CallToolError> {
        let inv = self.inventory().await?;
        let members = inv
            .resolve(&t.selector)
            .map_err(|e| tool_error(e.to_string()))?;
        let limit = members.join(",");
        Ok(json!({"members": members, "limit": limit}))
    }

    async fn tool_ping(&self, t: PingTool) -> Result<Value, CallToolError> {
        let inv = self.inventory().await?;
        let limit = inv
            .to_limit(&t.selector)
            .map_err(|e| tool_error(e.to_string()))?;
        let argv = vec![
            "ansible".to_string(),
            limit.clone(),
            "-m".to_string(),
            "ping".to_string(),
            "-o".to_string(),
            "-f".to_string(),
            t.forks.unwrap_or(15).max(1).to_string(),
            "-T".to_string(),
            t.connect_timeout.unwrap_or(10).max(1).to_string(),
        ];
        let out = self.run_adhoc(argv).await?;
        // Oneline format: "host | SUCCESS => {...}" / "host | UNREACHABLE! => …".
        // Parse regardless of exit code — a partial probe (some hosts down) is
        // the useful signal, and ansible returns non-zero whenever any host is
        // unreachable.
        let mut reachable = serde_json::Map::new();
        for line in out.stdout.lines() {
            let Some((host, rest)) = line.split_once('|') else {
                continue;
            };
            let host = host.trim();
            if host.is_empty() {
                continue;
            }
            let token = rest
                .trim()
                .split(' ')
                .next()
                .unwrap_or("")
                .trim_end_matches(['!', ':']);
            reachable.insert(host.to_string(), Value::Bool(token == "SUCCESS"));
        }
        let mut result = json!({
            "limit": limit,
            "reachable": reachable,
            "exit_code": out.code,
            // stdout only: stderr rides separately so warnings and side-band
            // noise (ansible relabels any subprocess stderr, e.g. git fetch
            // progress from an inventory eval, as [ERROR]) can't masquerade
            // as probe failures.
            "output": out.stdout.trim(),
        });
        let stderr = out.stderr.trim();
        if !stderr.is_empty() {
            result["diagnostics"] = Value::from(stderr);
        }
        Ok(result)
    }

    async fn tool_host_eval(&self, t: HostEvalTool) -> Result<Value, CallToolError> {
        let inv = self.inventory().await?;
        let Some(member) = inv.members().get(&t.member) else {
            return Err(tool_error(format!("no such member: {}", t.member)));
        };
        let metadata = serde_json::to_value(member).map_err(CallToolError::new)?;
        let mut result = json!({"member": t.member, "metadata": metadata, "toplevel": null});
        if t.toplevel.unwrap_or(false) {
            match self
                .state
                .effects
                .eval_expected(&self.state.flake, std::slice::from_ref(&t.member))
                .await
            {
                Ok(evaluated) => {
                    result["toplevel"] = evaluated
                        .get(&t.member)
                        .map_or(Value::Null, |p| Value::from(p.clone()));
                }
                Err(f) => {
                    result["eval_error"] =
                        failure_value(&format!("toplevel eval failed for {}", t.member), &f);
                }
            }
        }
        Ok(result)
    }

    async fn tool_drift(&self, t: DriftTool) -> Result<Value, CallToolError> {
        let inv = self.inventory().await?;
        let nodes = inv.deploy_nodes();

        let mut result = json!({"ok": true, "refreshed": false, "expected_source": "none"});

        if t.refresh.unwrap_or(false) {
            let survey = self
                .state
                .effects
                .refresh_snapshots()
                .await
                .map_err(|e| tool_error(e.to_string()))?;
            result["survey_rc"] = Value::from(survey.code);
            if survey.code == 0 {
                result["refreshed"] = Value::Bool(true);
            } else {
                result["ok"] = Value::Bool(false);
                result["refresh_error"] = json!({
                    "exit_code": survey.code,
                    "stdout": survey.stdout,
                    "stderr": survey.stderr,
                });
            }
        }

        let state_dir = drift::state_dir();
        let rev = self.state.effects.repo_rev(&self.state.flake).await;
        let (cached_rev, cached) = drift::load_expected(&state_dir);
        let mut expected: Option<BTreeMap<String, String>> = None;
        if t.do_eval.unwrap_or(false) {
            match self
                .state
                .effects
                .eval_expected(&self.state.flake, &nodes)
                .await
            {
                Ok(evaluated) => {
                    // Best-effort cache write; a read-only state dir must not
                    // sink an otherwise successful eval.
                    let _ = drift::save_expected(rev.as_deref(), &evaluated, &state_dir);
                    expected = Some(evaluated);
                    result["expected_source"] = Value::from("eval");
                }
                Err(f) => {
                    result["eval_error"] = failure_value("expected-toplevel eval failed", &f);
                }
            }
        } else if drift::cache_fresh(cached_rev.as_deref(), rev.as_deref()) {
            expected = Some(cached);
            result["expected_source"] = Value::from("cache");
        }

        let entries = drift::compare(
            &nodes,
            &drift::read_snapshots(&state_dir),
            expected.as_ref(),
            Some(drift::default_max_age()),
            chrono::Utc::now(),
        );
        result["rev"] = rev.map_or(Value::Null, Value::from);
        let dicts: Vec<Value> = entries
            .iter()
            .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
            .collect();
        let mut summary: BTreeMap<String, u64> = BTreeMap::new();
        for entry in &entries {
            *summary
                .entry(entry.status.as_str().to_string())
                .or_insert(0) += 1;
        }
        result["summary"] = serde_json::to_value(summary).map_err(CallToolError::new)?;
        result["total"] = Value::from(dicts.len());
        let filtered: Vec<Value> = match &t.statuses {
            Some(wanted) if !wanted.is_empty() => dicts
                .into_iter()
                .filter(|d| {
                    d.get("status")
                        .and_then(Value::as_str)
                        .is_some_and(|s| wanted.iter().any(|w| w == s))
                })
                .collect(),
            _ => dicts,
        };
        result["entries"] = Value::Array(filtered);
        Ok(result)
    }

    async fn tool_reload(&self) -> Result<Value, CallToolError> {
        if !self.state.reloadable {
            return Err(tool_error(
                "reload unavailable: this host cannot swap the inventory",
            ));
        }
        let fresh = self
            .state
            .effects
            .fresh_inventory(&self.state.flake)
            .await
            .map_err(|e| tool_error(e.to_string()))?;
        let members = fresh.members().len();
        let groups = fresh.groups().len();
        if let Ok(mut guard) = self.state.inventory.write() {
            *guard = Some(fresh);
        }
        Ok(json!({"ok": true, "members": members, "groups": groups}))
    }

    // ---- monitoring + action tiers ------------------------------------------

    async fn tool_deploy_status(&self, t: DeployStatusTool) -> Result<Value, CallToolError> {
        if let Some(run_id) = &t.run_id {
            if !registry::is_valid_run_id(run_id) {
                return Err(tool_error(format!(
                    "invalid run id {run_id:?}: expected a Mandala-generated run id"
                )));
            }
            let Some(mut obs) = registry::open_run(run_id) else {
                return Err(tool_error(format!("no such run: {run_id}")));
            };
            let wait = t.wait_seconds.unwrap_or(0).clamp(0, MAX_WAIT_SECONDS);
            #[allow(clippy::cast_sign_loss)]
            let deadline = Instant::now() + Duration::from_secs(wait as u64);
            let mut snap = run_snapshot(&mut obs);
            while snap.get("liveness").and_then(Value::as_str) == Some("running")
                && Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_secs(2)).await;
                snap = run_snapshot(&mut obs);
            }
            return Ok(snap);
        }
        #[allow(clippy::cast_sign_loss)]
        let limit = t.limit.unwrap_or(10).max(1) as usize;
        let mut runs = Vec::new();
        for info in registry::list_runs().into_iter().take(limit) {
            if let Some(mut obs) = registry::open_run(&info.run_id) {
                runs.push(run_snapshot(&mut obs));
            }
        }
        Ok(json!({"runs": runs}))
    }

    async fn tool_build(&self, t: BuildTool) -> Result<Value, CallToolError> {
        let inv = self.inventory().await?;
        let targets = inv
            .resolve(&t.selector)
            .map_err(|e| tool_error(e.to_string()))?;
        let mut argv = vec![
            "nix".to_string(),
            "build".to_string(),
            "--no-link".to_string(),
            "--print-out-paths".to_string(),
            "--no-warn-dirty".to_string(),
        ];
        argv.extend(targets.iter().map(|m| {
            let attr = serde_json::to_string(m).expect("member name is JSON serializable");
            format!(
                "{}#nixosConfigurations.{attr}.config.system.build.toplevel",
                self.state.flake
            )
        }));
        let mut extra = Meta::new();
        extra.insert("members".to_string(), Value::from(targets.clone()));
        let launch = self
            .state
            .effects
            .launch_command(argv, "build", None, extra)
            .await
            .map_err(|e| tool_error(e.to_string()))?;
        let base = json!({
            "run_id": launch.run_id,
            "members": targets,
            "log": launch.log.display().to_string(),
        });
        if !launch.launched {
            return Ok(merged(
                &base,
                json!({"ok": false, "error": "failed to launch nix — see log"}),
            ));
        }
        let Some(mut obs) = registry::open_run(&launch.run_id) else {
            return Err(tool_error(format!("no such run: {}", launch.run_id)));
        };
        let wait = t.wait_seconds.unwrap_or(120).clamp(0, MAX_WAIT_SECONDS);
        #[allow(clippy::cast_sign_loss)]
        let deadline = Instant::now() + Duration::from_secs(wait as u64);
        while obs.liveness() == RunLiveness::Running && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        if obs.liveness() == RunLiveness::Running {
            return Ok(merged(&base, json!({"building": true})));
        }
        let rc = obs.info.meta.get("rc").and_then(Value::as_i64);
        let lines = read_log_lines(&launch.log);
        if rc != Some(0) {
            let tail = lines
                .iter()
                .rev()
                .take(80)
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            return Ok(merged(
                &base,
                json!({
                    "ok": false, "exit_code": rc,
                    "error": "nix build failed",
                    "output": tail,
                }),
            ));
        }
        // The teed log interleaves nix's stderr chatter with the printed
        // out-paths; the out-paths are the unindented store paths.
        let out_paths: Vec<&String> = lines
            .iter()
            .filter(|l| l.starts_with("/nix/store/"))
            .collect();
        Ok(merged(&base, json!({"ok": true, "out_paths": out_paths})))
    }

    async fn tool_deploy(&self, t: DeployTool) -> Result<Value, CallToolError> {
        let inv = self.inventory().await?;
        let target = inv
            .to_limit(&t.selector)
            .map_err(|e| tool_error(e.to_string()))?;
        let dry_activate = t.dry_activate.unwrap_or(true);
        if !dry_activate && t.confirm.as_deref() != Some(target.as_str()) {
            return Ok(json!({
                "ok": false,
                "refused": true,
                "reason": "real activation requires `confirm` to equal the resolved target",
                "required_confirm": target,
                "dry_activate": dry_activate,
            }));
        }
        let launch = self
            .state
            .effects
            .launch_deploy(&self.state.flake, &target, dry_activate, DEPLOY_THROTTLE)
            .await
            .map_err(|e| tool_error(e.to_string()))?;
        Ok(json!({
            "ok": true,
            "run_id": launch.run_id,
            "limit": target,
            "dry_activate": dry_activate,
            "events_dir": launch.events_dir.display().to_string(),
        }))
    }

    async fn tool_restart_service(&self, t: RestartServiceTool) -> Result<Value, CallToolError> {
        let inv = self.inventory().await?;
        let target = inv
            .to_limit(&t.selector)
            .map_err(|e| tool_error(e.to_string()))?;
        if !valid_unit(&t.unit) {
            return Err(tool_error(format!(
                "not a plain systemd unit name: '{}'",
                t.unit
            )));
        }
        if t.confirm.as_deref() != Some(target.as_str()) {
            return Ok(json!({
                "ok": false,
                "refused": true,
                "reason": "restart_service requires `confirm` to equal the resolved target",
                "required_confirm": target,
                "unit": t.unit,
            }));
        }
        let argv = vec![
            "ansible".to_string(),
            target.clone(),
            "-m".to_string(),
            "ansible.builtin.systemd_service".to_string(),
            "-a".to_string(),
            format!("name={} state=restarted", t.unit),
            "-f".to_string(),
            t.forks.unwrap_or(4).max(1).to_string(),
        ];
        let out = self.run_adhoc(argv).await?;
        // Ad-hoc result lines: "host | CHANGED => {..." / "host | FAILED! => …";
        // parse regardless of exit code — the per-host map is the signal.
        let mut restarted = serde_json::Map::new();
        for line in out.stdout.lines() {
            let Some((host, rest)) = line.split_once(" | ") else {
                continue;
            };
            if host.is_empty() || host.contains(char::is_whitespace) {
                continue;
            }
            let status: String = rest.chars().take_while(char::is_ascii_uppercase).collect();
            if status.is_empty() {
                continue;
            }
            restarted.insert(
                host.to_string(),
                Value::Bool(status == "CHANGED" || status == "SUCCESS"),
            );
        }
        let mut result = json!({
            "ok": out.code == 0,
            "limit": target,
            "unit": t.unit,
            "restarted": restarted,
            "exit_code": out.code,
        });
        if out.code != 0 {
            result["output"] = Value::from(out.stdout.trim());
        }
        let stderr = out.stderr.trim();
        if !stderr.is_empty() {
            result["diagnostics"] = Value::from(stderr);
        }
        Ok(result)
    }

    async fn tool_reboot(&self, t: RebootTool) -> Result<Value, CallToolError> {
        let inv = self.inventory().await?;
        let target = inv
            .to_limit(&t.selector)
            .map_err(|e| tool_error(e.to_string()))?;
        let serial = t.serial.unwrap_or_else(|| "1".to_string());
        if !valid_serial(&serial) {
            // `-e "a=1 b=2"` sets MULTIPLE extra-vars — refuse anything but
            // a plain batch count / percentage before ansible parses it.
            return Err(tool_error(format!(
                "not a serial batch count or percentage: '{serial}'"
            )));
        }
        if t.confirm.as_deref() != Some(target.as_str()) {
            return Ok(json!({
                "ok": false,
                "refused": true,
                "reason": "reboot requires `confirm` to equal the resolved target",
                "required_confirm": target,
            }));
        }
        let drain = t.drain.unwrap_or(true);
        let Some(argv) = self.state.effects.reboot_argv(&target, &serial, drain) else {
            return Err(tool_error(
                "no ans-reboot wrapper or playbooks/reboot.yaml — reboot unavailable",
            ));
        };
        let program = argv[0].clone();
        let mut extra = Meta::new();
        extra.insert("limit".to_string(), Value::from(target.clone()));
        extra.insert("serial".to_string(), Value::from(serial.clone()));
        extra.insert("drain".to_string(), Value::from(drain));
        let launch = self
            .state
            .effects
            .launch_command(argv, "reboot", Some(ansible_dir()), extra)
            .await
            .map_err(|e| tool_error(e.to_string()))?;
        if !launch.launched {
            return Ok(json!({
                "ok": false,
                "error": format!("failed to launch {program} — see log"),
                "run_id": launch.run_id,
                "log": launch.log.display().to_string(),
            }));
        }
        Ok(json!({
            "ok": true,
            "run_id": launch.run_id,
            "limit": target,
            "serial": serial,
            "drain": drain,
            "log": launch.log.display().to_string(),
        }))
    }

    /// Run an ad-hoc argv through the effects, mapping spawn failures to the
    /// Python `ToolError` messages.
    async fn run_adhoc(&self, argv: Vec<String>) -> Result<AdhocOutput, CallToolError> {
        match self.state.effects.run_adhoc(argv).await {
            Ok(out) => Ok(out),
            Err(AdhocError::NotFound) => Err(tool_error("ansible not found on PATH")),
            Err(AdhocError::Other(msg)) => Err(tool_error(msg)),
        }
    }
}

/// Round to three decimals (Python `round(x, 3)` for `elapsed`).
fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Stamp the originating client on an activity event — only when there is
/// one (leader-local events must carry NO `origin` key, not a null).
fn with_origin(mut event: Value, origin: Option<&str>) -> Value {
    if let (Some(origin), Some(obj)) = (origin, event.as_object_mut()) {
        obj.insert("origin".to_string(), Value::from(origin));
    }
    event
}

/// Merge `extra`'s keys over `base` (both objects) — the Python `{**result,
/// …}` spread the build/reboot tools use.
fn merged(base: &Value, extra: Value) -> Value {
    let mut out = base.as_object().cloned().unwrap_or_default();
    if let Value::Object(extra) = extra {
        for (k, v) in extra {
            out.insert(k, v);
        }
    }
    Value::Object(out)
}

/// A tool's structured result as an MCP `CallToolResult`: the JSON rides both
/// as text content and as `structuredContent`.
fn to_call_result(value: Value) -> CallToolResult {
    let structured = value.as_object().cloned();
    CallToolResult {
        content: vec![TextContent::new(value.to_string(), None, None).into()],
        is_error: None,
        meta: None,
        structured_content: structured,
    }
}

/// Deserialize a tool's arguments, mapping a mismatch to a call error.
fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, CallToolError> {
    serde_json::from_value(value).map_err(CallToolError::new)
}

/// Read a teed log as lines (lossy UTF-8; missing file → empty).
fn read_log_lines(path: &std::path::Path) -> Vec<String> {
    std::fs::read(path)
        .map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .lines()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Per-host states + build progress for one registry run. A failed/rolled-
/// back host carries its raw stream so the client can debug it (the same text
/// the operator reads in the failed host tab). A command run (reboot, …) has
/// no event streams: its snapshot is liveness (pid, then the reaped rc) plus
/// the tail of its teed `output.log`.
fn run_snapshot(obs: &mut ObservedRun) -> Value {
    if obs.info.kind() != "deploy" {
        return command_snapshot(obs);
    }
    obs.poll();
    let mut hosts = serde_json::Map::new();
    for (name, h) in &obs.tailer.hosts {
        let mut entry = json!({
            "state": h.state.as_str(),
            "rc": h.rc,
            "milestones": h.milestones,
        });
        if matches!(h.state, HostState::Failed | HostState::RolledBack) {
            entry["raw"] = Value::from(h.lines.iter().cloned().collect::<Vec<_>>());
        }
        hosts.insert(name.clone(), entry);
    }
    let b = obs.tailer.build.clone();
    let liveness = obs.liveness();
    // A coarse phase so an early snapshot doesn't read as stalled: the
    // The native engine batch-builds every selected profile first (no host
    // events yet), then fans out per host.
    let phase = if liveness != RunLiveness::Running {
        "done"
    } else if hosts.is_empty() {
        "batch-build"
    } else {
        "fan-out"
    };
    json!({
        "run_id": obs.info.run_id,
        "kind": obs.info.kind(),
        "meta": Value::Object(obs.info.meta.clone()),
        "liveness": liveness.as_str(),
        "phase": phase,
        "hosts": hosts,
        "build": {
            "built": b.built,
            "finished": b.finished,
            "fetched": b.fetched,
            "errors": b.errors,
            "done": b.done,
            "rc": b.rc,
        },
    })
}

fn command_snapshot(obs: &mut ObservedRun) -> Value {
    let liveness = obs.liveness();
    let log_path = obs.info.path.join(COMMAND_LOG);
    let lines = read_log_lines(&log_path);
    let tail: Vec<&String> = lines.iter().rev().take(OUTPUT_TAIL).rev().collect();
    json!({
        "run_id": obs.info.run_id,
        "kind": obs.info.kind(),
        "meta": Value::Object(obs.info.meta.clone()),
        "liveness": liveness.as_str(),
        "phase": if liveness == RunLiveness::Running { "running" } else { "done" },
        "log": log_path.display().to_string(),
        "output_tail": tail,
    })
}

#[async_trait]
impl ServerHandler for MandalaHandler {
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
        self.call_tool(&params.name, params.arguments.unwrap_or_default())
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorded_run(emitter: &str) -> ObservedRun {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../mandala-core/tests/fixtures/deploy-runs")
            .join(emitter);
        ObservedRun {
            info: registry::RunInfo {
                run_id: format!("{emitter}-recording"),
                meta: registry::read_meta(&path),
                path: path.clone(),
            },
            tailer: mandala_core::runner::EventTailer::new(&path),
        }
    }

    fn normalize_emitter_fields(snapshot: &mut Value) {
        snapshot.as_object_mut().unwrap().remove("run_id");
        let meta = snapshot["meta"].as_object_mut().unwrap();
        for field in [
            "run_id",
            "started_at",
            "finished_at",
            "build_rc",
            "process_rc",
            "profiles",
            "summary",
        ] {
            meta.remove(field);
        }
    }

    #[test]
    fn unit_names_are_validated_to_plain_systemd_names() {
        for good in ["k3s", "nginx.service", "getty@tty1", "a:b_c-d.e"] {
            assert!(valid_unit(good), "{good} should be valid");
        }
        for bad in ["", "-lead", "/etc/passwd", "a b", "x;rm", "$(x)"] {
            assert!(!valid_unit(bad), "{bad} should be refused");
        }
    }

    #[test]
    fn serial_is_a_batch_count_or_percentage() {
        for good in ["1", "2", "100%", "05"] {
            assert!(valid_serial(good), "{good} should be valid");
        }
        // ansible parses `-e "a=1 b=2"` as MULTIPLE extra-vars — anything
        // beyond digits+optional-% is an injection vector.
        for bad in ["", "%", "1 drain=false", "1;x", "10%%", "-1"] {
            assert!(!valid_serial(bad), "{bad} should be refused");
        }
    }

    #[test]
    fn round3_matches_python_round() {
        assert_eq!(round3(1.234_567), 1.235);
        assert_eq!(round3(0.0004), 0.0);
    }

    /// MCP observes the same durable build/host state from both emitter paths.
    /// Native-only metadata is first pinned exactly, then only those explicit
    /// emitter fields and dynamic run identity/times are removed for parity;
    /// liveness, phase, rc, build counters, raw lines, and sticky states remain
    /// in the equality assertion.
    #[test]
    fn recorded_ansible_and_engine_runs_have_identical_mcp_snapshots() {
        let mut ansible = recorded_run("ansible");
        let mut engine = recorded_run("engine");
        let mut ansible_snapshot = run_snapshot(&mut ansible);
        let mut engine_snapshot = run_snapshot(&mut engine);

        assert_eq!(engine_snapshot["meta"]["build_rc"], 0);
        assert_eq!(engine_snapshot["meta"]["process_rc"], 0);
        assert_eq!(engine_snapshot["meta"]["rc"], 1);
        assert_eq!(
            engine_snapshot["meta"]["summary"],
            json!({"confirmed":1,"failed":0,"rolled_back":1,"total":2})
        );
        assert_eq!(
            engine_snapshot["meta"]["profiles"],
            json!({
                "alpha":"/nix/store/00000000000000000000000000000000-alpha-profile",
                "beta":"/nix/store/11111111111111111111111111111111-beta-profile",
            })
        );

        normalize_emitter_fields(&mut ansible_snapshot);
        normalize_emitter_fields(&mut engine_snapshot);
        assert_eq!(engine_snapshot, ansible_snapshot);
        assert_eq!(engine_snapshot["liveness"], "rolled-back");
        assert_eq!(engine_snapshot["phase"], "done");
        assert_eq!(engine_snapshot["hosts"]["alpha"]["state"], "confirmed");
        assert_eq!(engine_snapshot["hosts"]["beta"]["state"], "rolled-back");
        assert_eq!(
            engine_snapshot["hosts"]["beta"]["raw"],
            json!(["confirmation failed; rolled back"])
        );
    }
}
