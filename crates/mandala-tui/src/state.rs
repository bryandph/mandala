//! Pure-data explorer state — the render-visible half of the strict
//! `AppState`/`App` split, and the whole `explorer.py` read tier as PURE
//! transitions.
//!
//! Nothing in here may hold a handle: no terminal, no channels, no child
//! processes, no tasks. Transitions mutate state and RETURN what background
//! work should start ([`LoadRequest`] / booleans); the runtime ([`crate::
//! app`] / [`crate::explorer`]) spawns tokio tasks for them and feeds
//! results back through [`crate::event::AppEvent`]. That split is what makes
//! the status machinery — sticky errors, queued reloads, concurrent
//! eval+survey — drivable from tests without a runtime, terminal, or fleet.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::{DateTime, Utc};
use mandala_core::drift::{self, DriftStatus, Snapshot};
use mandala_core::inventory::Inventory;
use serde_json::Value;

use crate::screen::ScreenState;
use crate::scroll::ScrollState;
use crate::select::SelectTable;

/// Retained settled-call lines in the mcp activity log (the Python RichLog
/// `max_lines=2000`).
pub const MCP_LOG_MAX: usize = 2000;

/// The session's role in the fleet context, for the subtle status indicator
/// and the self-call filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextRole {
    /// This process hosts the context endpoint.
    Leader,
    /// Attached to another process's leader.
    Observer,
}

/// One in-flight context call (start seen, settle pending) — the
/// `_mcp_pending` analog: it rides the status-bar spinner as `mcp <tool>`
/// and one line of the panel's pending strip until its settle pops it.
#[derive(Debug, Clone)]
pub struct McpPending {
    pub tool: String,
    /// The originating client (absent for the serving leader's own calls).
    pub origin: Option<String>,
    /// Pre-formatted `k=v` argument pairs (the Python `f"{k}={v!r}"` join).
    pub args: String,
}

/// One settled call in the activity log — rendered as
/// `▸ tool  ⟨origin⟩  args  [ok · 3.2s]` (+ red detail on error).
#[derive(Debug, Clone)]
pub struct McpLogEntry {
    pub tool: String,
    pub origin: Option<String>,
    pub args: String,
    /// `ok`/`error`, `· <elapsed>s`-suffixed when the settle carried one.
    pub label: String,
    pub ok: bool,
    pub detail: Option<String>,
}

/// What the runtime must do after an activity settle (the imperative tail
/// of the Python `_on_mcp_activity`, returned as data so the transition
/// stays pure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpFollowUp {
    /// A client's deploy/reboot launched: attach its run — exact `run_id`
    /// from the settle's result summary, or (older events) the newest live
    /// run of `kind`.
    Attach {
        kind: String,
        run_id: Option<String>,
    },
    /// A client's `drift(refresh/do_eval)` landed: re-read the shared
    /// snapshots/expected cache exactly like an operator S-refresh landing.
    DriftLanded,
    /// A client's `reload` swapped the leader's inventory: re-read the
    /// contract through the context (the eval already happened — cheap).
    ReloadLanded,
}

/// Braille spinner frames advanced by the tick timer while any job runs.
/// One shared frame animates every running job at once (the Python
/// `_SPINNER`).
pub const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// The explorer's tabs. Hand-rolled tab bar + view switch (ratatui has no
/// TabbedContent) — the enum IS the active-view state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Members,
    Groups,
    Drift,
}

impl Tab {
    /// Tab-bar order.
    pub const ALL: [Tab; 3] = [Tab::Members, Tab::Groups, Tab::Drift];

    /// The tab-bar label.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Tab::Members => "members",
            Tab::Groups => "groups",
            Tab::Drift => "drift",
        }
    }

    /// The next tab in bar order, wrapping.
    #[must_use]
    pub fn next(self) -> Tab {
        match self {
            Tab::Members => Tab::Groups,
            Tab::Groups => Tab::Drift,
            Tab::Drift => Tab::Members,
        }
    }

    /// The previous tab in bar order, wrapping.
    #[must_use]
    pub fn prev(self) -> Tab {
        match self {
            Tab::Members => Tab::Drift,
            Tab::Groups => Tab::Members,
            Tab::Drift => Tab::Groups,
        }
    }
}

/// One members-tab row (cells beside the [`SelectTable`] name registry).
#[derive(Debug, Clone)]
pub struct MemberRow {
    pub name: String,
    pub platform: String,
    pub arch: String,
    pub category: String,
    pub role: String,
    pub tags: String,
    pub surfaces: String,
}

/// One groups-tab row.
#[derive(Debug, Clone)]
pub struct GroupRow {
    pub name: String,
    pub n: String,
    pub members: String,
}

/// One drift-tab row. `status` stays the CORE vocabulary — the style is
/// applied at render time through the one mapping in [`crate::render`].
#[derive(Debug, Clone)]
pub struct DriftRow {
    pub name: String,
    pub status: DriftStatus,
    pub current: String,
    pub expected: String,
    pub booted: String,
    pub captured: String,
}

/// What the background load task produced: the evaluated inventory plus the
/// drift-cache inspection the Python `_load` worker performs alongside it.
#[derive(Debug, Clone)]
pub struct LoadedInventory {
    pub inventory: Inventory,
    /// `drift::repo_rev(flake)` at load time.
    pub rev: Option<String>,
    /// The rev the `.expected.json` cache was evaluated at.
    pub cached_rev: Option<String>,
    /// The cached expected toplevels (adopted only when the cache is fresh).
    pub cached: BTreeMap<String, String>,
}

/// A load the runtime should start. Carries the inventory GENERATION the
/// result must match — the stale-aggregate guard (`_fill`'s `inv is not
/// self.inventory` identity check): a fill for a superseded inventory must
/// not paint — and whether the CONTRACT must be refreshed first (`r`):
/// locally a load always evaluates fresh, but a context-routed load serves
/// the leader's cached inventory unless it runs the `reload` tool first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadRequest {
    pub generation: u64,
    /// This load must re-evaluate the contract (a reload, not a first read).
    pub fresh: bool,
}

/// The whole render-visible state. Pure data; `Clone` on purpose so tests
/// can fork scenarios cheaply.
#[derive(Debug, Clone)]
pub struct AppState {
    pub tab: Tab,
    pub members_table: SelectTable,
    pub member_rows: Vec<MemberRow>,
    pub groups_table: SelectTable,
    pub group_rows: Vec<GroupRow>,
    pub drift_table: SelectTable,
    pub drift_rows: Vec<DriftRow>,
    /// The drift tab's bottom hint line (keys + the expected-cache caption).
    pub drift_hint: String,

    /// The evaluated inventory; `None` until the first load lands (and again
    /// after `r` rebinds a fresh unevaluated one) — which is exactly why
    /// [`AppState::deploy_nodes`] never forces an eval: unevaluated → empty.
    pub inventory: Option<Inventory>,
    /// Locally evaluated expected toplevels (from the fresh cache or an
    /// `S` eval); `None` = never evaluated for this contract.
    pub expected: Option<BTreeMap<String, String>>,
    /// The contract's rev at the last load/eval.
    pub rev: Option<String>,
    /// The rev the expected cache was evaluated at.
    pub cached_rev: Option<String>,

    /// Inventory generation, bumped by every reload. A landing
    /// [`crate::event::AppEvent::LoadFinished`] with a stale generation is
    /// dropped, not painted.
    pub generation: u64,
    /// An aggregate/expected eval is in flight (`_busy`): gates BOTH the
    /// load and the expected eval, and queues reloads.
    pub busy: bool,
    /// A reload arrived while busy — queued, not dropped; consumed by the
    /// fill/fail/drift-done paths.
    pub reload_pending: bool,
    /// The state survey is in flight (`_surveying`); independent of `busy`.
    pub surveying: bool,
    /// Fresh snapshots counted so far this survey run.
    pub survey_n: usize,

    /// Resting status-bar text shown when no job is running.
    pub status: String,
    /// An error holds until the next refresh begins; a concurrently
    /// finishing success never overwrites it.
    pub status_sticky: bool,
    /// Current spinner frame index (mod [`SPINNER_FRAMES`]).
    pub spin: usize,

    /// Whether this session renders MCP call monitoring (`--debug-mcp`).
    /// Gates the activity panel, the pending strip, the status-bar
    /// `mcp <tool>` jobs, and the `m` hint/binding — the `check_action`
    /// conditional-visibility mechanism. The activity SUBSCRIPTION itself is
    /// flag-independent (settle events drive normal operation).
    pub debug_mcp: bool,
    /// The activity panel's visibility (the `m` toggle; meaningful only
    /// under `debug_mcp`). Defaults shown, like the Python panel.
    pub mcp_panel: bool,
    /// In-flight context calls by `seq` (the `_mcp_pending` dict).
    pub mcp_pending: BTreeMap<u64, McpPending>,
    /// Settled-call lines for the activity panel, capped at
    /// [`MCP_LOG_MAX`]. Recorded flag-independently (cheap), rendered only
    /// under `debug_mcp`.
    pub mcp_log: VecDeque<McpLogEntry>,
    pub mcp_scroll: ScrollState,
    /// This session's role in the fleet context (`None` = no context — the
    /// local-eval fallback shape).
    pub context_role: Option<ContextRole>,
    /// Our hello identity in the context (`tui-<pid>`) — the self-filter
    /// key: our own calls are already represented by the explorer's job
    /// flags and never double-render as activity.
    pub mcp_client: Option<String>,
    /// Runs already auto-attached this session (`_attached_runs`): a settle
    /// naming an already-attached run attaches nothing.
    pub attached_runs: BTreeSet<String>,
    /// A contract refresh is owed to the next load (see
    /// [`LoadRequest::fresh`]); set by [`AppState::request_reload`] and
    /// consumed by [`AppState::request_load`], so a reload queued behind a
    /// busy worker still refreshes when it finally runs.
    pub fresh_wanted: bool,

    /// The action tier's ONE screen/overlay slot (section-5 decision of
    /// record: the Python explorer never stacks two screens — every modal
    /// dismisses before the next push — so an `Option` models exactly the
    /// reachable states; a stack would model impossible ones). Modals
    /// render over the explorer; task/deploy screens replace the view.
    pub screen: Option<ScreenState>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    /// The empty pre-load state (the explorer before `on_mount`'s `_load`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            tab: Tab::Members,
            members_table: SelectTable::default(),
            member_rows: Vec::new(),
            groups_table: SelectTable::default(),
            group_rows: Vec::new(),
            drift_table: SelectTable::default(),
            drift_rows: Vec::new(),
            drift_hint: String::new(),
            inventory: None,
            expected: None,
            rev: None,
            cached_rev: None,
            generation: 0,
            busy: false,
            reload_pending: false,
            surveying: false,
            survey_n: 0,
            status: String::new(),
            status_sticky: false,
            spin: 0,
            debug_mcp: false,
            mcp_panel: true,
            mcp_pending: BTreeMap::new(),
            mcp_log: VecDeque::new(),
            mcp_scroll: ScrollState::default(),
            context_role: None,
            mcp_client: None,
            attached_runs: BTreeSet::new(),
            fresh_wanted: false,
            screen: None,
        }
    }

    /// The active tab's select table.
    #[must_use]
    pub fn active_table(&self) -> &SelectTable {
        match self.tab {
            Tab::Members => &self.members_table,
            Tab::Groups => &self.groups_table,
            Tab::Drift => &self.drift_table,
        }
    }

    /// Mutable form of [`AppState::active_table`] (key handling).
    pub fn active_table_mut(&mut self) -> &mut SelectTable {
        match self.tab {
            Tab::Members => &mut self.members_table,
            Tab::Groups => &mut self.groups_table,
            Tab::Drift => &mut self.drift_table,
        }
    }

    /// The action target on the active tab: the multi-selection when one
    /// exists, else the cursor row; names comma-joined into an ansible
    /// `--limit` (the Python `_target`). Feeds the section-5 action tier.
    #[must_use]
    pub fn target(&self) -> Option<String> {
        let table = self.active_table();
        let selected = table.selected_names();
        if !selected.is_empty() {
            return Some(selected.join(","));
        }
        table.cursor_name().map(str::to_string)
    }

    /// The action target expressed in Mandala's selector algebra. Ansible
    /// accepts taxonomy group names directly, while Mandala deliberately
    /// reserves the `@group` spelling so a group cannot be mistaken for a
    /// member. Member and drift rows already contain member names; only the
    /// groups tab needs translating before a native deploy is launched.
    #[must_use]
    pub fn deploy_target(&self) -> Option<String> {
        let target = self.target()?;
        if self.tab != Tab::Groups {
            return Some(target);
        }
        Some(
            target
                .split(',')
                .map(|group| format!("@{group}"))
                .collect::<Vec<_>>()
                .join(","),
        )
    }

    // -- status machinery ------------------------------------------------
    //
    // eval and survey run CONCURRENTLY (refresh_drift fires both) and the
    // survey usually finishes first. Each job owns a running flag; while any
    // are set the bar lists every job still running behind ONE shared
    // spinner frame. With all jobs idle the bar shows the latest resting
    // message (a result, or a sticky error).

    /// Set the resting bar message. Errors are sticky: a concurrently
    /// finishing success will not overwrite them; the stickiness clears when
    /// the next refresh begins (the Python `_set_status`).
    pub fn set_status(&mut self, msg: impl Into<String>, error: bool) {
        if error || !self.status_sticky {
            self.status = msg.into();
            self.status_sticky = error;
        }
    }

    /// The jobs currently running, as spinner-line labels. Under
    /// `--debug-mcp`, in-flight context calls join the list as `mcp <tool>`
    /// (the `_mcp_pending` analog: a client-launched drift eval spins in the
    /// bar exactly as if the operator had pressed S); without the flag no
    /// monitoring surface exists — not even here.
    #[must_use]
    pub fn jobs(&self) -> Vec<String> {
        let mut jobs = Vec::new();
        if self.busy {
            jobs.push("eval".to_string());
        }
        if self.surveying {
            jobs.push(if self.survey_n > 0 {
                format!("survey ({} read)", self.survey_n)
            } else {
                "survey".to_string()
            });
        }
        if self.debug_mcp {
            for pending in self.mcp_pending.values() {
                jobs.push(format!("mcp {}", pending.tool));
            }
        }
        jobs
    }

    /// Whether any background job is running (keeps the spinner timer armed).
    #[must_use]
    pub fn any_job_running(&self) -> bool {
        self.busy || self.surveying || (self.debug_mcp && !self.mcp_pending.is_empty())
    }

    /// The status line (the Python `sub_title`): the running-jobs spinner
    /// line while any job runs, else the resting message.
    #[must_use]
    pub fn status_line(&self) -> String {
        let jobs = self.jobs();
        if jobs.is_empty() {
            return self.status.clone();
        }
        let frame = SPINNER_FRAMES[self.spin % SPINNER_FRAMES.len()];
        let spun: Vec<String> = jobs.iter().map(|j| format!("{frame} {j}")).collect();
        format!("running   {}", spun.join("   ·   "))
    }

    // -- load / reload ---------------------------------------------------

    /// Start (or queue) an aggregate load. A reload while a worker runs is
    /// QUEUED, not silently dropped (the Python `_load`).
    #[must_use]
    pub fn request_load(&mut self) -> Option<LoadRequest> {
        if self.busy {
            self.reload_pending = true;
            return None;
        }
        self.busy = true;
        Some(LoadRequest {
            generation: self.generation,
            fresh: std::mem::take(&mut self.fresh_wanted),
        })
    }

    /// `r`: rebind a fresh (unevaluated) inventory, drop the expected set,
    /// and load — the returned request (if any) evaluates the NEW contract;
    /// an in-flight eval keeps its old generation and will not paint. The
    /// `fresh` mark survives queueing, so a reload consumed later still
    /// refreshes the contract (context loads run the `reload` tool first).
    #[must_use]
    pub fn request_reload(&mut self) -> Option<LoadRequest> {
        self.generation += 1;
        self.inventory = None;
        self.expected = None;
        self.fresh_wanted = true;
        self.request_load()
    }

    /// Run a reload queued while a worker was busy (`_consume_pending_reload`).
    #[must_use]
    fn consume_pending_reload(&mut self) -> Option<LoadRequest> {
        if self.reload_pending && !self.busy {
            self.reload_pending = false;
            return self.request_load();
        }
        None
    }

    /// A load task settled. Stale-generation results (superseded by a
    /// reload) are dropped without painting; the queued reload repaints from
    /// the fresh inventory. Returns a follow-up load to start, if one was
    /// queued.
    #[must_use]
    pub fn on_load_finished(
        &mut self,
        generation: u64,
        result: Result<LoadedInventory, String>,
        snapshots: &BTreeMap<String, Snapshot>,
        now: DateTime<Utc>,
    ) -> Option<LoadRequest> {
        if generation != self.generation {
            // The contract was reloaded while this eval ran — don't paint
            // the stale aggregate (the Python `_fill` identity check).
            self.busy = false;
            return self.consume_pending_reload();
        }
        match result {
            Err(error) => {
                self.busy = false;
                let last = error.lines().next_back().unwrap_or("unknown").to_string();
                let last = if last.is_empty() {
                    "unknown".into()
                } else {
                    last
                };
                self.set_status(format!("aggregate eval failed: {last}"), true);
                self.consume_pending_reload()
            }
            Ok(loaded) => self.fill(loaded, snapshots, now),
        }
    }

    /// Paint a freshly evaluated inventory into every view (`_fill`).
    fn fill(
        &mut self,
        loaded: LoadedInventory,
        snapshots: &BTreeMap<String, Snapshot>,
        now: DateTime<Utc>,
    ) -> Option<LoadRequest> {
        let LoadedInventory {
            inventory,
            rev,
            cached_rev,
            cached,
        } = loaded;
        self.rev = rev;
        self.cached_rev = cached_rev;
        // Reuse the rev-keyed expected cache when the contract hasn't moved
        // since the last eval (a mismatch is itself the signal).
        if drift::cache_fresh(self.cached_rev.as_deref(), self.rev.as_deref()) {
            self.expected = Some(cached);
        }

        self.member_rows = inventory
            .members()
            .iter()
            .map(|(name, m)| MemberRow {
                name: name.clone(),
                platform: field_or(m.get("platform"), "?"),
                arch: field_or(m.get("architecture"), "?"),
                category: field_or(m.get("category"), "?"),
                role: role_or_dash(m.get("role")),
                tags: join_tags(m.get("tags")),
                surfaces: m.surfaces(),
            })
            .collect();
        self.members_table
            .reset_rows(self.member_rows.iter().map(|r| r.name.clone()).collect());

        self.group_rows = inventory
            .groups()
            .iter()
            .map(|(group, names)| {
                let mut sorted = names.clone();
                sorted.sort();
                GroupRow {
                    name: group.clone(),
                    n: names.len().to_string(),
                    members: sorted.join(" "),
                }
            })
            .collect();
        self.groups_table
            .reset_rows(self.group_rows.iter().map(|r| r.name.clone()).collect());

        let n_members = inventory.members().len();
        let n_groups = inventory.groups().len();
        self.inventory = Some(inventory);
        self.fill_drift(snapshots, now);
        self.busy = false;
        self.set_status(
            format!(
                "{n_members} members, {n_groups} groups — space/shift+↑↓ select · p ping · R reboot · D deploy"
            ),
            false,
        );
        self.consume_pending_reload()
    }

    /// The deploy-projection node names — NEVER forcing an eval: a
    /// just-reloaded (unevaluated) inventory reports no nodes until its
    /// background eval lands (the Python `_deploy_nodes`).
    #[must_use]
    pub fn deploy_nodes(&self) -> Vec<String> {
        self.inventory
            .as_ref()
            .map(Inventory::deploy_nodes)
            .unwrap_or_default()
    }

    /// Rebuild the drift table + hint from the current snapshots/expected
    /// state (`_fill_drift`). Pure over its inputs: the runtime reads the
    /// snapshot dir and the clock, tests inject fixtures.
    pub fn fill_drift(&mut self, snapshots: &BTreeMap<String, Snapshot>, now: DateTime<Utc>) {
        let nodes = self.deploy_nodes();
        let entries = drift::compare(
            &nodes,
            snapshots,
            self.expected.as_ref(),
            Some(drift::default_max_age()),
            now,
        );
        self.drift_rows = entries
            .into_iter()
            .map(|e| DriftRow {
                current: short_store(e.current.as_deref()),
                expected: short_store(e.expected.as_deref()),
                booted: short_store(e.booted.as_deref()),
                captured: e
                    .captured_at
                    .as_deref()
                    .unwrap_or("")
                    .chars()
                    .take(19)
                    .collect(),
                status: e.status,
                name: e.host,
            })
            .collect();
        self.drift_table
            .reset_rows(self.drift_rows.iter().map(|r| r.name.clone()).collect());
        self.drift_hint = drift_hint(
            self.expected.is_some(),
            self.rev.as_deref(),
            self.cached_rev.as_deref(),
        );
    }

    // -- S: concurrent expected eval + survey ----------------------------

    /// `S`: refresh both drift inputs at once — the expected-toplevel eval
    /// and the read-only state survey run CONCURRENTLY, each refreshing the
    /// drift table as it lands; either completion order converges. A fresh
    /// refresh clears a stale sticky error. Returns (start eval?, start
    /// survey?).
    #[must_use]
    pub fn refresh_drift(&mut self) -> (bool, bool) {
        self.status_sticky = false;
        (self.request_eval_expected(), self.request_survey())
    }

    /// Start the expected-toplevel eval unless an eval-class job already
    /// runs (`action_eval_expected` returns early on `_busy` — no queueing).
    #[must_use]
    pub fn request_eval_expected(&mut self) -> bool {
        if self.busy {
            return false;
        }
        self.busy = true;
        true
    }

    /// The expected eval settled (`_drift_done`): adopt the result (an
    /// error drops the expected set), repaint drift, surface the outcome
    /// (errors sticky), and run any queued reload.
    #[must_use]
    pub fn on_drift_eval_finished(
        &mut self,
        result: Result<(Option<String>, BTreeMap<String, String>), String>,
        snapshots: &BTreeMap<String, Snapshot>,
        now: DateTime<Utc>,
    ) -> Option<LoadRequest> {
        self.busy = false;
        let error = match result {
            Ok((rev, expected)) => {
                self.rev = rev.clone();
                self.cached_rev = rev;
                self.expected = Some(expected);
                None
            }
            Err(e) => {
                self.expected = None;
                Some(e)
            }
        };
        self.fill_drift(snapshots, now);
        // The survey spinner stays up if it is still counting, so this
        // resting message only surfaces once both jobs are idle; an eval
        // error is sticky and wins over the survey's success message.
        match error {
            Some(e) => self.set_status(e, true),
            None => self.set_status("drift refreshed", false),
        }
        self.consume_pending_reload()
    }

    /// Start the state survey unless one already runs (`action_survey`).
    #[must_use]
    pub fn request_survey(&mut self) -> bool {
        if self.surveying {
            return false;
        }
        self.surveying = true;
        self.survey_n = 0;
        true
    }

    /// The live fresh-snapshot tally moved (`_survey_progress`).
    pub fn on_survey_progress(&mut self, n: usize) {
        self.survey_n = n;
    }

    /// The survey settled (`_survey_done`): repaint drift from the fresh
    /// snapshots and surface the outcome (failures sticky).
    pub fn on_survey_done(
        &mut self,
        n: usize,
        rc: i32,
        error: Option<&str>,
        snapshots: &BTreeMap<String, Snapshot>,
        now: DateTime<Utc>,
    ) {
        self.surveying = false;
        self.survey_n = n;
        self.fill_drift(snapshots, now);
        if rc == 0 {
            let plural = if n == 1 { "" } else { "s" };
            self.set_status(
                format!("drift refreshed · surveyed {n} host{plural}"),
                false,
            );
        } else {
            let msg = format!("survey failed (exit {rc}): {}", error.unwrap_or(""));
            self.set_status(msg.trim_end().to_string(), true);
        }
    }

    /// A mutation screen (deploy/reboot task, attached run) just closed —
    /// the `_after_mutation` rule: a completed run (rc set — even non-zero:
    /// seeing the resulting state is exactly what you want) auto-refreshes
    /// drift; an operator cancel (rc `None`) does not. Returns the
    /// (eval, survey) jobs to start, exactly like `S`.
    #[must_use]
    pub fn after_mutation(&mut self, rc: Option<i64>) -> (bool, bool) {
        if rc.is_some() {
            self.refresh_drift()
        } else {
            (false, false)
        }
    }

    /// Advance the spinner. Returns whether anything visible changed (only
    /// while a job runs — an idle tick must not dirty the frame).
    pub fn tick_spinner(&mut self) -> bool {
        if self.any_job_running() {
            self.spin = self.spin.wrapping_add(1);
            true
        } else {
            false
        }
    }

    // -- context activity ------------------------------------------------
    //
    // The `_on_mcp_activity` port, adapted to the context model: events
    // arrive from the context subscription (whoever serves the call), not an
    // in-process middleware. One deliberate adaptation: the TUI's OWN calls
    // (its context-routed loads and evals) are skipped entirely — they are
    // already represented by the explorer's job flags, and rendering them as
    // activity would double every spinner and re-fire the drift-landed
    // refresh after the TUI's own S handling.

    /// Whether an activity event describes one of OUR calls: as leader our
    /// own dispatches carry no origin (every wire call does); as observer
    /// ours carry our hello identity (the leader's own carry none and must
    /// render).
    #[must_use]
    fn is_own_call(&self, origin: Option<&str>) -> bool {
        match self.context_role {
            Some(ContextRole::Leader) => origin.is_none(),
            Some(ContextRole::Observer) => origin.is_some() && origin == self.mcp_client.as_deref(),
            None => false,
        }
    }

    /// One activity event from the context subscription. Pure bookkeeping —
    /// pending strip in/out, the settled log line — plus the follow-up the
    /// runtime must run (auto-attach / drift-landed / reload swap), returned
    /// as data.
    pub fn on_mcp_activity(&mut self, event: &Value) -> Option<McpFollowUp> {
        let origin = event.get("origin").and_then(Value::as_str);
        if self.is_own_call(origin) {
            return None;
        }
        let tool = event
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        let status = event.get("status").and_then(Value::as_str).unwrap_or("?");
        let seq = event.get("seq").and_then(Value::as_u64);
        let args = event.get("args").cloned().unwrap_or(Value::Null);
        let arg_str = format_mcp_args(&args);
        if status == "start" {
            // The call is now PENDING: it spins in the status bar (like an
            // operator-launched eval/survey) and in the panel's pending
            // strip until its ok/error event pops it.
            if let Some(seq) = seq {
                self.mcp_pending.insert(
                    seq,
                    McpPending {
                        tool,
                        origin: origin.map(str::to_string),
                        args: arg_str,
                    },
                );
            }
            return None;
        }
        if let Some(seq) = seq {
            self.mcp_pending.remove(&seq);
        }
        let ok = status == "ok";
        let mut label = status.to_string();
        if let Some(elapsed) = event.get("elapsed").and_then(Value::as_f64) {
            label.push_str(&format!(" · {elapsed:.1}s"));
        }
        let detail = event
            .get("detail")
            .and_then(Value::as_str)
            .filter(|d| !d.is_empty())
            .map(str::to_string);
        self.mcp_log.push_back(McpLogEntry {
            tool: tool.clone(),
            origin: origin.map(str::to_string),
            args: arg_str,
            label,
            ok,
            detail,
        });
        while self.mcp_log.len() > MCP_LOG_MAX {
            self.mcp_log.pop_front();
        }
        self.mcp_scroll.update_content(self.mcp_log.len());
        if !ok {
            return None;
        }
        // A client-launched run renders like a human one: attach the exact
        // run the settle's result summary names (a refused call launches
        // nothing but still settles ok — result carries refused, no attach);
        // without a summary (older events) fall back to the newest live run
        // of that kind.
        if tool == "deploy" || tool == "reboot" {
            return match event.get("result") {
                None | Some(Value::Null) => Some(McpFollowUp::Attach {
                    kind: tool,
                    run_id: None,
                }),
                Some(res) => {
                    if res.get("ok").and_then(Value::as_bool) == Some(true)
                        && let Some(run_id) = res
                            .get("run_id")
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty())
                    {
                        Some(McpFollowUp::Attach {
                            kind: tool,
                            run_id: Some(run_id.to_string()),
                        })
                    } else {
                        None
                    }
                }
            };
        }
        if tool == "drift" && (truthy(args.get("refresh")) || truthy(args.get("do_eval"))) {
            return Some(McpFollowUp::DriftLanded);
        }
        if tool == "reload" {
            return Some(McpFollowUp::ReloadLanded);
        }
        None
    }

    /// A client's `drift(refresh/do_eval)` call wrote fresh snapshots and/or
    /// the expected cache into the SHARED state dir — adopt them exactly as
    /// if the operator's own S-refresh had landed (`_mcp_drift_landed`). The
    /// inputs (repo rev, cache, snapshots) are read at the runtime edge.
    pub fn on_mcp_drift_landed(
        &mut self,
        rev: Option<String>,
        cached_rev: Option<String>,
        cached: BTreeMap<String, String>,
        snapshots: &BTreeMap<String, Snapshot>,
        now: DateTime<Utc>,
    ) {
        self.rev = rev;
        self.cached_rev = cached_rev;
        if drift::cache_fresh(self.cached_rev.as_deref(), self.rev.as_deref()) {
            self.expected = Some(cached);
        }
        self.fill_drift(snapshots, now);
        self.set_status("drift refreshed (mcp)", false);
    }
}

/// Python truthiness over a JSON value (the `args.get("refresh")` check).
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// The activity line's argument string: `k=v` pairs joined by spaces — the
/// Python `" ".join(f"{k}={v!r}")`, with repr approximated for JSON values
/// (strings single-quoted, booleans/None Python-spelled, the rest as JSON).
#[must_use]
pub fn format_mcp_args(args: &Value) -> String {
    let Some(obj) = args.as_object() else {
        return String::new();
    };
    obj.iter()
        .map(|(k, v)| format!("{k}={}", py_repr(v)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn py_repr(value: &Value) -> String {
    match value {
        Value::String(s) => format!("'{s}'"),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Null => "None".to_string(),
        other => other.to_string(),
    }
}

/// The drift tab's hint line — keys plus the exact three expected-cache
/// captions from the Python `_fill_drift`. Deliberately NOT
/// `mandala_core::cli::drift_caption`: that vocabulary says "pass --eval"
/// (a CLI flag); the TUI's says "press S" (a key). Two surfaces, two
/// spellings of the same three states, each pinned by its own test.
#[must_use]
pub fn drift_hint(expected_known: bool, rev: Option<&str>, cached_rev: Option<&str>) -> String {
    let mut hint = "S refresh drift (survey + eval) · R reboot a reboot-pending row".to_string();
    if expected_known {
        hint.push_str(&format!("   expected @ {}", drift::short_rev(rev)));
    } else if cached_rev.is_some() {
        hint.push_str(&format!(
            "   contract MOVED since last eval (cache @ {}, repo @ {}) — press S",
            drift::short_rev(cached_rev),
            drift::short_rev(rev),
        ));
    } else {
        hint.push_str("   (expected NOT evaluated yet — press S)");
    }
    hint
}

/// A member field as its string value, or `default` when absent/non-string
/// (Python `m.get(key, "?")`).
fn field_or(value: Option<&serde_json::Value>, default: &str) -> String {
    value
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| default.to_string(), str::to_string)
}

/// The `role` column: the role string, or `-` when absent/empty (Python
/// `m.get("role") or "-"`).
fn role_or_dash(value: Option<&serde_json::Value>) -> String {
    match value.and_then(serde_json::Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => "-".to_string(),
    }
}

/// The `tags` column: tag strings space-joined (Python `" ".join(...)`).
fn join_tags(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

/// Shorten a store path the explorer's way: strip `/nix/store/`, keep 18
/// chars; `None` renders empty (the Python `_fill_drift` `short` lambda —
/// distinct from the CLI table's 20-char `-`-defaulted `short_store`).
fn short_store(path: Option<&str>) -> String {
    let p = path.unwrap_or("");
    let p = p.strip_prefix("/nix/store/").unwrap_or(p);
    p.chars().take(18).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_spinner_tick_is_not_a_visible_change() {
        let mut s = AppState::new();
        assert!(!s.tick_spinner());
        s.busy = true;
        assert!(s.tick_spinner());
        assert_eq!(s.spin, 1);
    }

    #[test]
    fn tab_cycle_wraps_both_ways() {
        assert_eq!(Tab::Members.next(), Tab::Groups);
        assert_eq!(Tab::Drift.next(), Tab::Members);
        assert_eq!(Tab::Members.prev(), Tab::Drift);
        for tab in Tab::ALL {
            assert_eq!(tab.next().prev(), tab);
        }
    }

    #[test]
    fn short_store_matches_the_explorer_lambda() {
        assert_eq!(
            short_store(Some("/nix/store/abcdefghijklmnopqrstuvwxyz")),
            "abcdefghijklmnopqr"
        );
        assert_eq!(short_store(None), "");
        assert_eq!(short_store(Some("bare")), "bare");
    }

    #[test]
    fn sticky_error_semantics() {
        let mut s = AppState::new();
        s.set_status("eval failed: boom", true);
        s.set_status("drift refreshed · surveyed 3 hosts", false);
        assert_eq!(s.status, "eval failed: boom"); // success never stomps an error
        s.set_status("worse", true);
        assert_eq!(s.status, "worse"); // a newer error does replace it
        s.status_sticky = false;
        s.set_status("ok now", false);
        assert_eq!(s.status, "ok now");
    }

    // ---- context activity (the `_on_mcp_activity` port) ---------------------

    use serde_json::json;

    fn observer(debug: bool) -> AppState {
        let mut s = AppState::new();
        s.context_role = Some(ContextRole::Observer);
        s.mcp_client = Some("tui-1".to_string());
        s.debug_mcp = debug;
        s
    }

    fn start_event(tool: &str, seq: u64, origin: Option<&str>) -> Value {
        let mut e =
            json!({"tool": tool, "args": {}, "status": "start", "detail": null, "seq": seq});
        if let Some(o) = origin {
            e["origin"] = Value::from(o);
        }
        e
    }

    fn settle_event(tool: &str, seq: u64, origin: Option<&str>, extra: Value) -> Value {
        let mut e = json!({
            "tool": tool, "args": {}, "status": "ok", "detail": null,
            "seq": seq, "elapsed": 3.24,
        });
        if let Some(o) = origin {
            e["origin"] = Value::from(o);
        }
        for (k, v) in extra.as_object().into_iter().flatten() {
            e[k] = v.clone();
        }
        e
    }

    #[test]
    fn pending_calls_ride_the_status_bar_only_under_debug_mcp() {
        let mut s = observer(true);
        assert!(
            s.on_mcp_activity(&start_event("drift", 7, Some("mcp-9")))
                .is_none()
        );
        assert!(s.jobs().contains(&"mcp drift".to_string()));
        assert!(
            s.any_job_running(),
            "a pending call keeps the spinner armed"
        );
        // Without the flag: no monitoring surface — not even the bar.
        let mut quiet = observer(false);
        assert!(
            quiet
                .on_mcp_activity(&start_event("drift", 7, Some("mcp-9")))
                .is_none()
        );
        assert!(quiet.jobs().is_empty());
        assert!(!quiet.any_job_running());
        // The settle pops the pending strip either way.
        assert!(
            s.on_mcp_activity(&settle_event("drift", 7, Some("mcp-9"), json!({})))
                .is_none()
        );
        assert!(s.mcp_pending.is_empty());
        assert!(!s.any_job_running());
    }

    #[test]
    fn settle_line_carries_the_python_label_format() {
        let mut s = observer(true);
        let event = settle_event(
            "resolve",
            1,
            Some("mcp-9"),
            json!({"args": {"selector": "@k3s", "full": true}}),
        );
        let _ = s.on_mcp_activity(&event);
        let entry = s.mcp_log.back().expect("logged");
        assert_eq!(entry.tool, "resolve");
        assert_eq!(entry.origin.as_deref(), Some("mcp-9"));
        assert_eq!(entry.label, "ok · 3.2s");
        assert!(entry.ok);
        assert_eq!(entry.args, "full=True selector='@k3s'");
        // An error settle: bold-red label + red detail.
        let mut err = settle_event("deploy", 2, Some("mcp-9"), json!({}));
        err["status"] = Value::from("error");
        err["detail"] = Value::from("no such member: db");
        let _ = s.on_mcp_activity(&err);
        let entry = s.mcp_log.back().expect("logged");
        assert!(!entry.ok);
        assert_eq!(entry.label, "error · 3.2s");
        assert_eq!(entry.detail.as_deref(), Some("no such member: db"));
    }

    #[test]
    fn own_calls_are_skipped_role_dependently() {
        // Observer: our own origin is skipped; the leader's no-origin calls
        // and other clients' calls render.
        let mut s = observer(true);
        assert!(
            s.on_mcp_activity(&start_event("members", 1, Some("tui-1")))
                .is_none()
        );
        assert!(s.mcp_pending.is_empty(), "own call must not double-render");
        let _ = s.on_mcp_activity(&start_event("members", 2, None));
        let _ = s.on_mcp_activity(&start_event("members", 3, Some("mcp-4")));
        assert_eq!(
            s.mcp_pending.len(),
            2,
            "leader-local + other clients render"
        );
        // Leader: OUR calls are the no-origin ones.
        let mut l = observer(true);
        l.context_role = Some(ContextRole::Leader);
        assert!(
            l.on_mcp_activity(&start_event("members", 1, None))
                .is_none()
        );
        assert!(l.mcp_pending.is_empty());
        let _ = l.on_mcp_activity(&start_event("members", 2, Some("mcp-4")));
        assert_eq!(l.mcp_pending.len(), 1);
    }

    #[test]
    fn settle_follow_ups_match_the_python_dispatch() {
        let mut s = observer(false);
        // deploy ok + run_id: exact attach.
        let f = s.on_mcp_activity(&settle_event(
            "deploy",
            1,
            Some("mcp-9"),
            json!({"result": {"ok": true, "run_id": "r-1"}}),
        ));
        assert_eq!(
            f,
            Some(McpFollowUp::Attach {
                kind: "deploy".into(),
                run_id: Some("r-1".into())
            })
        );
        // No result summary (older events): fall back to the newest of kind.
        let f = s.on_mcp_activity(&settle_event("reboot", 2, Some("mcp-9"), json!({})));
        assert_eq!(
            f,
            Some(McpFollowUp::Attach {
                kind: "reboot".into(),
                run_id: None
            })
        );
        // A refused call launches nothing.
        let f = s.on_mcp_activity(&settle_event(
            "deploy",
            3,
            Some("mcp-9"),
            json!({"result": {"ok": false, "refused": true}}),
        ));
        assert_eq!(f, None);
        // drift with refresh/do_eval lands the shared state.
        let f = s.on_mcp_activity(&settle_event(
            "drift",
            4,
            Some("mcp-9"),
            json!({"args": {"do_eval": true}}),
        ));
        assert_eq!(f, Some(McpFollowUp::DriftLanded));
        let f = s.on_mcp_activity(&settle_event(
            "drift",
            5,
            Some("mcp-9"),
            json!({"args": {"statuses": ["drift"]}}),
        ));
        assert_eq!(f, None, "a plain drift read lands nothing");
        // reload swaps the inventory.
        let f = s.on_mcp_activity(&settle_event("reload", 6, Some("mcp-9"), json!({})));
        assert_eq!(f, Some(McpFollowUp::ReloadLanded));
        // An error settle never fires a follow-up.
        let mut err = settle_event(
            "deploy",
            7,
            Some("mcp-9"),
            json!({"result": {"ok": true, "run_id": "r-2"}}),
        );
        err["status"] = Value::from("error");
        assert_eq!(s.on_mcp_activity(&err), None);
    }

    #[test]
    fn mcp_drift_landed_adopts_a_fresh_cache_only() {
        let mut s = observer(false);
        let mut cached = BTreeMap::new();
        cached.insert("web".to_string(), "/nix/store/aaa".to_string());
        s.on_mcp_drift_landed(
            Some("r1".into()),
            Some("r1".into()),
            cached.clone(),
            &BTreeMap::new(),
            Utc::now(),
        );
        assert_eq!(s.expected.as_ref(), Some(&cached));
        assert_eq!(s.status, "drift refreshed (mcp)");
        // A moved contract does not adopt the stale cache.
        let mut s = observer(false);
        s.on_mcp_drift_landed(
            Some("r2".into()),
            Some("r1".into()),
            cached,
            &BTreeMap::new(),
            Utc::now(),
        );
        assert!(s.expected.is_none());
    }

    #[test]
    fn reload_request_marks_fresh_and_the_mark_survives_queueing() {
        let mut s = AppState::new();
        let req = s.request_load().expect("initial load");
        assert!(!req.fresh, "a first read serves the cached contract");
        // Reload while busy: queued, and still fresh when consumed.
        let mut s = AppState::new();
        let _ = s.request_load();
        assert!(
            s.request_reload().is_none(),
            "queued behind the busy worker"
        );
        assert!(s.reload_pending && s.fresh_wanted);
        s.busy = false;
        s.reload_pending = false; // consume path runs request_load directly
        let req = s.request_load().expect("consumed reload");
        assert!(req.fresh, "the queued reload still refreshes the contract");
    }

    #[test]
    fn mcp_log_is_capped() {
        let mut s = observer(false);
        for seq in 0..(MCP_LOG_MAX as u64 + 5) {
            let _ = s.on_mcp_activity(&settle_event("ping", seq, Some("mcp-9"), json!({})));
        }
        assert_eq!(s.mcp_log.len(), MCP_LOG_MAX);
    }

    #[test]
    fn format_mcp_args_approximates_python_repr() {
        assert_eq!(
            format_mcp_args(
                &json!({"selector": "@k3s", "dry_activate": false, "forks": 3, "x": null})
            ),
            "dry_activate=False forks=3 selector='@k3s' x=None"
        );
        assert_eq!(format_mcp_args(&json!({})), "");
        assert_eq!(format_mcp_args(&Value::Null), "");
    }
}
