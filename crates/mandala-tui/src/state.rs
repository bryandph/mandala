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

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use mandala_core::drift::{self, DriftStatus, Snapshot};
use mandala_core::inventory::Inventory;

use crate::screen::ScreenState;
use crate::select::SelectTable;

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
/// not paint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadRequest {
    pub generation: u64,
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

    /// Whether this session renders MCP call monitoring (`--debug-mcp`,
    /// section 6). Only the footer-hint plumbing reads it here — the
    /// `check_action`-style conditional-visibility mechanism.
    pub debug_mcp: bool,

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

    /// The jobs currently running, as spinner-line labels. MCP pending
    /// calls join this list in section 6 (the `_mcp_pending` analog).
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
        jobs
    }

    /// Whether any background job is running (keeps the spinner timer armed).
    #[must_use]
    pub fn any_job_running(&self) -> bool {
        self.busy || self.surveying
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
        })
    }

    /// `r`: rebind a fresh (unevaluated) inventory, drop the expected set,
    /// and load — the returned request (if any) evaluates the NEW contract;
    /// an in-flight eval keeps its old generation and will not paint.
    #[must_use]
    pub fn request_reload(&mut self) -> Option<LoadRequest> {
        self.generation += 1;
        self.inventory = None;
        self.expected = None;
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
}
