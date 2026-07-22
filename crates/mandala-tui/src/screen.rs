//! The action tier's pushed screens as PURE data + render fns — the
//! `tui/tasks.py` + `tui/deploy.py` (view half) port (tasks 5.1/5.2/5.4).
//!
//! There is no Textual `ModalScreen`/`Screen` machinery here. The shape of
//! record (section-5 design decision): [`crate::state::AppState`] holds ONE
//! `Option<ScreenState>` overlay slot — the Python explorer never stacks two
//! screens (every modal dismisses before the next push), so a stack would
//! model states that cannot occur. Dismissal continuations are DATA, not
//! callbacks: a confirm carries its [`ConfirmAction`], and the task/attached/
//! deploy screens carry `after_mutation: bool` — the runtime reads them at
//! dismissal and fires the drift auto-refresh when the rc is `Some` (the
//! `_after_mutation` rule: rc `None` = operator cancel, no refresh).
//!
//! Everything in this module is pure over its inputs, EXCEPT
//! [`attached_pump`]/[`attached_close_rc`], which read the run registry —
//! they sit here (beside the state they mutate) the way `fresh_snapshots`
//! sits in `explorer.rs`: the deterministic, directly-testable IO unit.
//! Subprocess/pty handles never appear here — they live in `app.rs` /
//! `deploy.rs` (the strict `AppState`/`App` split).

use std::collections::{BTreeMap, VecDeque};

use mandala_core::registry::{self, RunLiveness};
use mandala_core::runner::{COMMAND_LOG, EventTailer, HostState};
use nix_build_forest::{ForestSnapshot, ForestWidget};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::ansi;
#[cfg(test)]
use crate::render::rich_style;
use crate::scroll::ScrollState;
use crate::theme::Theme;

/// Scrollback kept by the task/attached log screens (Python
/// `deque(maxlen=8000)` / `RichLog(max_lines=8000)`).
pub const SCROLLBACK_MAX: usize = 8000;

/// The reboot-unavailable status message, verbatim from `action_reboot`.
pub const REBOOT_UNAVAILABLE: &str =
    "no ans-reboot wrapper or playbooks/reboot.yaml — reboot task unavailable";

/// Append to a capacity-bounded deque, dropping the oldest when full.
fn push_capped<T>(dq: &mut VecDeque<T>, item: T, cap: usize) {
    if dq.len() >= cap {
        dq.pop_front();
    }
    dq.push_back(item);
}

/// Format an optional rc the way Python f-strings render it (`None` / `3`).
fn fmt_rc(rc: Option<i64>) -> String {
    rc.map_or_else(|| "None".to_string(), |v| v.to_string())
}

/// The one screen/overlay slot over the explorer.
#[derive(Debug, Clone)]
pub enum ScreenState {
    /// Modal over the explorer: y / esc gate for destructive actions.
    Confirm(ConfirmState),
    /// Modal over the explorer: reboot batch order + drain safety.
    Reboot(RebootState),
    /// Full screen: one subprocess, streamed output.
    Task(TaskState),
    /// Full screen: read-only tail of a registered run's `output.log`.
    AttachedLog(AttachedLogState),
    /// Full screen: the deploy runner.
    Deploy(Box<DeployViewState>),
}

// ==== ConfirmScreen ==========================================================

/// What a confirmed modal runs — the dismissal continuation as data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    /// `D`: launch the fan-out deploy of `target`.
    Deploy { target: String },
}

/// The y/esc modal (`ConfirmScreen`).
#[derive(Debug, Clone)]
pub struct ConfirmState {
    pub message: String,
    pub action: ConfirmAction,
}

impl ConfirmState {
    #[must_use]
    pub fn new(message: impl Into<String>, action: ConfirmAction) -> Self {
        Self {
            message: message.into(),
            action,
        }
    }
}

/// The confirm modal's text, exactly the Python `compose` Text: the message
/// (bold), a blank line, then `y to run   esc to cancel`.
#[must_use]
pub fn confirm_lines(state: &ConfirmState) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = state
        .message
        .split('\n')
        .map(|part| {
            Line::from(Span::styled(
                part.to_string(),
                Style::new().add_modifier(Modifier::BOLD),
            ))
        })
        .collect();
    lines.push(Line::default());
    lines.push(run_cancel_line());
    lines
}

/// The shared `y to run   esc to cancel` trailer (both modals end with it).
fn run_cancel_line() -> Line<'static> {
    Line::from(vec![
        Span::styled(
            "y",
            Style::new()
                .fg(ratatui::style::Color::Red)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" to run   "),
        Span::styled("esc", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(" to cancel"),
    ])
}

// ==== RebootScreen ===========================================================

/// `(key, label, playbook serial value)` — the `_ORDERS` table verbatim.
/// serial: 1 one-at-a-time, 2 rolling, "100%" every targeted host at once
/// (0 is rejected by modern ansible, 100% is the portable "all in one
/// batch").
pub const ORDERS: [(char, &str, &str); 3] = [
    ('1', "Serial — one host at a time", "1"),
    ('2', "Rolling — 2 hosts in flight", "2"),
    ('3', "All-at-once — every targeted host together", "100%"),
];

/// The reboot options modal (`RebootScreen`): number keys pick the order,
/// `d` toggles drain, `y` runs.
#[derive(Debug, Clone)]
pub struct RebootState {
    pub target: String,
    pub order: char,
    pub drain: bool,
}

/// The `y` dismissal payload: `{serial, drain}` for `reboot_argv`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebootChoice {
    pub serial: &'static str,
    pub drain: bool,
}

impl RebootState {
    #[must_use]
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            order: '1',
            drain: true,
        }
    }

    /// A number key picks the order (unknown keys are ignored).
    pub fn set_order(&mut self, key: char) {
        if ORDERS.iter().any(|(k, _, _)| *k == key) {
            self.order = key;
        }
    }

    /// `d` flips the drain toggle (default ON).
    pub fn toggle_drain(&mut self) {
        self.drain = !self.drain;
    }

    /// The `y` payload: the chosen order's playbook serial + drain flag.
    #[must_use]
    pub fn choice(&self) -> RebootChoice {
        let serial = ORDERS
            .iter()
            .find(|(k, _, _)| *k == self.order)
            .map(|(_, _, s)| *s)
            .unwrap_or("1");
        RebootChoice {
            serial,
            drain: self.drain,
        }
    }
}

/// The reboot modal's text, the exact `_refresh` rendering: radio glyphs
/// (`●`/`○`), bold-green/dim states, the drain toggle's two captions, and
/// the y/esc trailer.
#[must_use]
pub fn reboot_lines(state: &RebootState) -> Vec<Line<'static>> {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let dim = Style::new().add_modifier(Modifier::DIM);
    let bold_green = Style::new()
        .fg(ratatui::style::Color::Green)
        .add_modifier(Modifier::BOLD);

    let mut lines = vec![
        Line::from(Span::styled(format!("Reboot '{}'?", state.target), bold)),
        Line::default(),
        Line::from(vec![Span::styled("Order ", bold), Span::raw("(1/2/3)")]),
    ];
    for (key, label, _) in ORDERS {
        let on = key == state.order;
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                if on { "●" } else { "○" },
                if on { bold_green } else { dim },
            ),
            Span::styled(format!(" {label}"), if on { bold } else { dim }),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("k8s ", bold),
        Span::raw("(d)"),
    ]));
    let drain_label = if state.drain {
        " Drain-safe: cordon & drain k8s nodes first"
    } else {
        " Skip drain: reboot k8s nodes without draining"
    };
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            if state.drain { "●" } else { "○" },
            if state.drain { bold_green } else { dim },
        ),
        Span::styled(drain_label, if state.drain { bold } else { dim }),
    ]));
    lines.push(Line::default());
    lines.push(run_cancel_line());
    lines
}

// ==== TaskScreen =============================================================

/// One subprocess, streamed output, exit code — the `TaskScreen` state. The
/// runtime (`App`) owns the child; this holds only what renders.
#[derive(Debug, Clone)]
pub struct TaskState {
    pub title: String,
    /// Routes subprocess events to THIS screen: a dismissed task's late
    /// lines must not leak into a newer one.
    pub task_id: u64,
    pub lines: VecDeque<String>,
    pub scroll: ScrollState,
    /// The exit code once the subprocess settled; `None` while running or
    /// when the launch failed — exactly the dismissal payload (`None` = no
    /// completed run = no drift refresh).
    pub rc: Option<i64>,
    /// Whether the subprocess spawned at all.
    pub launched: bool,
    /// Fire the drift auto-refresh on a completed dismissal (reboot: yes;
    /// ping: no — the Python `push_screen` callback presence).
    pub after_mutation: bool,
}

impl TaskState {
    #[must_use]
    pub fn new(title: impl Into<String>, task_id: u64, after_mutation: bool) -> Self {
        Self {
            title: title.into(),
            task_id,
            lines: VecDeque::new(),
            scroll: ScrollState::default(),
            rc: None,
            launched: false,
            after_mutation,
        }
    }

    pub fn push_line(&mut self, line: String) {
        push_capped(&mut self.lines, line, SCROLLBACK_MAX);
        self.scroll.update_content(self.lines.len());
    }

    /// The subprocess settled: record the rc and append the exit trailer.
    pub fn on_exited(&mut self, rc: i64) {
        self.rc = Some(rc);
        self.push_line(format!("— exit {rc}"));
    }
}

// ==== AttachedLogScreen ======================================================

/// A tailed log line: raw output (through the ANSI helper at render) or an
/// injected liveness notice (styled directly, the Python `Text(style=…)`).
#[derive(Debug, Clone)]
pub enum LogLine {
    Raw(String),
    Notice { text: String, error: bool },
}

/// Read-only observer of a registered run: tail `output.log` by byte
/// offset, report liveness from the registry. Never owns the subprocess —
/// esc detaches, the run keeps going.
#[derive(Debug, Clone)]
pub struct AttachedLogState {
    pub title: String,
    pub run_id: String,
    pub offset: u64,
    pub lines: VecDeque<LogLine>,
    pub scroll: ScrollState,
    pub settled: bool,
    pub after_mutation: bool,
}

impl AttachedLogState {
    #[must_use]
    pub fn new(title: impl Into<String>, run_id: impl Into<String>, after_mutation: bool) -> Self {
        Self {
            title: title.into(),
            run_id: run_id.into(),
            offset: 0,
            lines: VecDeque::new(),
            scroll: ScrollState::default(),
            settled: false,
            after_mutation,
        }
    }
}

/// One poll of the attached run (the Python `_pump`, 0.5s cadence): read the
/// log tail from the recorded byte offset, then judge liveness once —
/// gone → a pruned notice, settled → the `— <liveness> (rc=…)` trailer.
pub fn attached_pump(state: &mut AttachedLogState) {
    let Some(mut obs) = registry::open_run(&state.run_id) else {
        if !state.settled {
            state.settled = true;
            push_capped(
                &mut state.lines,
                LogLine::Notice {
                    text: format!("run {} is gone (pruned?)", state.run_id),
                    error: true,
                },
                SCROLLBACK_MAX,
            );
            state.scroll.update_content(state.lines.len());
        }
        return;
    };
    let path = obs.info.path.join(COMMAND_LOG);
    let chunk = read_from_offset(&path, &mut state.offset);
    for line in chunk.lines() {
        push_capped(
            &mut state.lines,
            LogLine::Raw(line.to_string()),
            SCROLLBACK_MAX,
        );
    }
    if state.settled {
        return;
    }
    let liveness = obs.liveness();
    if liveness != RunLiveness::Running {
        state.settled = true;
        let rc = obs.info.meta.get("rc").and_then(serde_json::Value::as_i64);
        push_capped(
            &mut state.lines,
            LogLine::Notice {
                text: format!("— {} (rc={})", liveness.as_str(), fmt_rc(rc)),
                error: liveness != RunLiveness::Finished,
            },
            SCROLLBACK_MAX,
        );
    }
    state.scroll.update_content(state.lines.len());
}

/// The esc payload (the Python `action_close`): an observer never terminates
/// the run; hand back its rc only once it has settled (`None` while still
/// running) so the after-mutation hook fires only for a completed run.
#[must_use]
pub fn attached_close_rc(run_id: &str) -> Option<i64> {
    let mut obs = registry::open_run(run_id)?;
    if obs.liveness() == RunLiveness::Running {
        return None;
    }
    obs.info.meta.get("rc").and_then(serde_json::Value::as_i64)
}

/// Tail-read a file from `*offset`, advancing it by the bytes read
/// (the Python `seek`/`read`/`tell`; decode errors are replaced).
fn read_from_offset(path: &std::path::Path, offset: &mut u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    if file.seek(SeekFrom::Start(*offset)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let Ok(n) = file.read_to_end(&mut buf) else {
        return String::new();
    };
    *offset += n as u64;
    String::from_utf8_lossy(&buf).into_owned()
}

// ==== DeployScreen (view state) ==============================================

/// The deploy screen's per-host state style — the `_STATE_STYLE` table
/// verbatim, as rich-style token specs through the one [`rich_style`]
/// mapper. The match is exhaustive over [`HostState`], so a new state
/// cannot ship unstyled (compile-enforced).
#[must_use]
pub fn host_state_style_spec(state: HostState) -> &'static str {
    match state {
        HostState::Pending => "dim",
        HostState::Evaluating | HostState::Building => "cyan",
        HostState::Copying => "blue",
        HostState::Activating | HostState::Waiting => "yellow",
        HostState::Confirmed => "green",
        HostState::RolledBack | HostState::Failed => "bold red",
    }
}

/// The ratatui style for a host state (never unmapped: the spec vocabulary
/// is covered by `rich_style`, gated by the exhaustiveness test).
#[must_use]
pub fn host_state_style(state: HostState) -> Style {
    Theme::default().host_state(state)
}

/// The `_STATE_GLYPH` table verbatim.
#[must_use]
pub fn host_state_glyph(state: HostState) -> &'static str {
    match state {
        HostState::Pending => "○",
        HostState::Evaluating => "…",
        HostState::Building => "⚙",
        HostState::Copying => "⇄",
        HostState::Activating => "⚡",
        HostState::Waiting => "⏳",
        HostState::Confirmed => "✓",
        HostState::RolledBack => "↩",
        HostState::Failed => "✗",
    }
}

/// The deploy screen's tab identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployTab {
    /// The native build-forest pane (`⚙ build`).
    Build,
    /// The playbook's own stdout/stderr mirror (`ansible`).
    Playbook,
    /// One host's demuxed stream, labeled `<glyph> <name>`.
    Host(String),
    /// The exit summary (materialized once, on finish).
    Summary,
}

/// One host tab's render data, snapshot from the tailer each poll.
#[derive(Debug, Clone)]
pub struct HostTabState {
    pub name: String,
    pub state: HostState,
    pub rc: Option<i64>,
    pub lines: Vec<String>,
}

/// The summary tab's data (the `_show_summary` construction, materialized
/// exactly once).
#[derive(Debug, Clone)]
pub struct SummaryState {
    /// `deploy succeeded` / `deploy FAILED (exit rc)`.
    pub head: String,
    pub ok: bool,
    /// `   -l <limit>   <m>m<ss>s` + `   dry-activate`.
    pub meta: String,
    /// `batch build: built …, fetched …, errors …, rc …`.
    pub build_line: String,
    /// Styled red when the build rc is bad (not 0/None).
    pub build_bad: bool,
    /// Host table rows (sorted by name).
    pub hosts: Vec<(String, HostState, Option<i64>)>,
    /// ansible's own per-host accounting, verbatim from the output mirror
    /// (everything from the `PLAY RECAP` line on).
    pub recap: Vec<String>,
}

/// The deploy screen's render-visible state (`DeployScreen` minus the run
/// handles, which live in [`crate::deploy::DeployJob`]).
#[derive(Debug, Clone)]
pub struct DeployViewState {
    pub limit: String,
    pub dry_activate: bool,
    /// Standalone (`mandala tui deploy`): esc exits the app with the rc.
    pub standalone: bool,
    /// Attached: the run was launched elsewhere — esc detaches, never
    /// terminates (the run keeps going).
    pub attached: bool,
    pub after_mutation: bool,
    pub active: DeployTab,
    pub build_line: String,
    pub forest: Option<Box<ForestSnapshot>>,
    pub build_scroll: ScrollState,
    pub playbook_lines: Vec<String>,
    pub playbook_scroll: ScrollState,
    /// Sorted by name (the tailer's BTreeMap order).
    pub hosts: Vec<HostTabState>,
    pub host_scrolls: BTreeMap<String, ScrollState>,
    pub finished: bool,
    pub returncode: Option<i64>,
    pub summary: Option<SummaryState>,
}

impl DeployViewState {
    #[must_use]
    pub fn new(
        limit: impl Into<String>,
        dry_activate: bool,
        standalone: bool,
        attached: bool,
        after_mutation: bool,
    ) -> Self {
        Self {
            limit: limit.into(),
            dry_activate,
            standalone,
            attached,
            after_mutation,
            active: DeployTab::Build,
            build_line: String::new(),
            forest: None,
            build_scroll: ScrollState::default(),
            playbook_lines: Vec::new(),
            playbook_scroll: ScrollState::default(),
            hosts: Vec::new(),
            host_scrolls: BTreeMap::new(),
            finished: false,
            returncode: None,
            summary: None,
        }
    }

    /// The sub-title (`-l <limit>` + ` (dry-activate)` + ` — exit <rc>` once
    /// finished — the `on_mount`/`_tick` sub_title composition).
    #[must_use]
    pub fn sub_title(&self) -> String {
        let mut title = format!("-l {}", self.limit);
        if self.dry_activate {
            title.push_str(" (dry-activate)");
        }
        if self.finished {
            title.push_str(&format!(" — exit {}", fmt_rc(self.returncode)));
        }
        title
    }

    /// One poll's view refresh (the `_tick` body, minus the run handles):
    /// build line, playbook mirror, host tabs (sorted, appearing as events
    /// arrive), and — exactly once, on the finish transition — the summary
    /// tab materializes and takes focus.
    pub fn sync(
        &mut self,
        tailer: Option<&EventTailer>,
        output: &[String],
        finished: bool,
        returncode: Option<i64>,
        elapsed_secs: u64,
    ) {
        if let Some(tailer) = tailer {
            self.forest = Some(Box::new(tailer.forest.snapshot()));
            let forest_len = self
                .forest
                .as_deref()
                .map_or(0, ForestWidget::activity_line_count);
            self.build_scroll.update_content(forest_len);
            let b = &tailer.build;
            let mut head = format!(
                "batch build  built {}/{}  fetched {}/{}  errors {}",
                b.finished, b.built, b.fetched_done, b.fetched, b.errors
            );
            if b.done {
                head.push_str(&format!("  —  done rc={}", fmt_rc(b.rc)));
            } else if !b.current.is_empty() {
                head.push_str(&format!("  —  {}", b.current));
            }
            self.build_line = head;
            self.hosts = tailer
                .hosts
                .values()
                .map(|h| HostTabState {
                    name: h.name.clone(),
                    state: h.state,
                    rc: h.rc,
                    lines: h.lines.iter().cloned().collect(),
                })
                .collect();
            for host in &self.hosts {
                self.host_scrolls
                    .entry(host.name.clone())
                    .or_default()
                    .update_content(host.lines.len());
            }
        }
        self.playbook_lines = output.to_vec();
        self.playbook_scroll
            .update_content(self.playbook_lines.len());
        if finished && !self.finished {
            self.finished = true;
            self.returncode = returncode;
            self.summary = Some(self.make_summary(tailer, elapsed_secs));
            self.active = DeployTab::Summary;
        }
    }

    pub fn active_scroll_mut(&mut self) -> Option<&mut ScrollState> {
        match &self.active {
            DeployTab::Build => Some(&mut self.build_scroll),
            DeployTab::Playbook => Some(&mut self.playbook_scroll),
            DeployTab::Host(name) => self.host_scrolls.get_mut(name),
            DeployTab::Summary => None,
        }
    }

    /// Build the summary tab data (`_show_summary`).
    fn make_summary(&self, tailer: Option<&EventTailer>, elapsed_secs: u64) -> SummaryState {
        let rc = self.returncode;
        let ok = rc == Some(0);
        let head = if ok {
            "deploy succeeded".to_string()
        } else {
            format!("deploy FAILED (exit {})", fmt_rc(rc))
        };
        let (minutes, seconds) = (elapsed_secs / 60, elapsed_secs % 60);
        let mut meta = format!("   -l {}   {minutes}m{seconds:02}s", self.limit);
        if self.dry_activate {
            meta.push_str("   dry-activate");
        }
        let (build_line, build_bad) = match tailer {
            Some(t) => {
                let b = &t.build;
                (
                    format!(
                        "batch build: built {}/{}, fetched {}/{}, errors {}, rc {}",
                        b.finished,
                        b.built,
                        b.fetched_done,
                        b.fetched,
                        b.errors,
                        fmt_rc(b.rc)
                    ),
                    !matches!(b.rc, Some(0) | None),
                )
            }
            None => (String::new(), false),
        };
        let recap_at = self
            .playbook_lines
            .iter()
            .position(|l| l.contains("PLAY RECAP"));
        let recap = recap_at.map_or_else(Vec::new, |i| self.playbook_lines[i..].to_vec());
        SummaryState {
            head,
            ok,
            meta,
            build_line,
            build_bad,
            hosts: self
                .hosts
                .iter()
                .map(|h| (h.name.clone(), h.state, h.rc))
                .collect(),
            recap,
        }
    }
}

/// The deploy screen's tab-bar order: build, ansible, hosts (sorted), then
/// summary once it exists.
#[must_use]
pub fn deploy_tabs(view: &DeployViewState) -> Vec<DeployTab> {
    let mut tabs = vec![DeployTab::Build, DeployTab::Playbook];
    tabs.extend(view.hosts.iter().map(|h| DeployTab::Host(h.name.clone())));
    if view.summary.is_some() {
        tabs.push(DeployTab::Summary);
    }
    tabs
}

/// The next/previous tab from the active one, wrapping (keyboard-first tab
/// cycling — the Textual TabbedContent was mouse-navigable; without a mouse
/// the host tabs need a cycle key).
#[must_use]
pub fn cycle_tab(view: &DeployViewState, delta: i64) -> DeployTab {
    let tabs = deploy_tabs(view);
    let at = tabs.iter().position(|t| *t == view.active).unwrap_or(0) as i64;
    let n = tabs.len() as i64;
    let next = (at + delta).rem_euclid(n) as usize;
    tabs[next].clone()
}

// ==== rendering ==============================================================

/// A centered overlay rect (`align: center middle`), clamped to the area.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Render a modal box (bordered, cleared background) with its text lines.
fn render_modal(frame: &mut Frame, lines: Vec<Line<'static>>, width: u16, theme: &Theme) {
    let area = frame.area();
    // +2 borders; width capped at 90% of the screen (the CSS max-width).
    let box_w = width.min(area.width * 9 / 10);
    let box_h = (lines.len() as u16 + 2).min(area.height);
    let rect = centered(area, box_w, box_h);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().border_style(theme.modal)),
        rect,
    );
}

/// The confirm modal over whatever is behind it.
pub fn render_confirm(state: &ConfirmState, frame: &mut Frame, theme: &Theme) {
    render_modal(frame, confirm_lines(state), 70, theme);
}

/// The reboot options modal.
pub fn render_reboot(state: &RebootState, frame: &mut Frame, theme: &Theme) {
    render_modal(frame, reboot_lines(state), 76, theme);
}

/// Full-screen layout for the task/attached screens: header, log, footer.
fn screen_chrome(area: Rect) -> [Rect; 3] {
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area)
}

fn render_header(frame: &mut Frame, area: Rect, title: &str, sub: &str, theme: &Theme) {
    let mut spans = vec![Span::styled(format!("mandala — {title}"), theme.header)];
    if !sub.is_empty() {
        spans.push(Span::styled(format!("   {sub}"), theme.footer_label));
    }
    frame.render_widget(Line::from(spans), area);
}

fn render_footer_hint(frame: &mut Frame, area: Rect, hints: &[(&str, &str)], theme: &Theme) {
    let mut spans = Vec::new();
    for (i, (key, label)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ·  ", theme.footer_label));
        }
        spans.push(Span::styled((*key).to_string(), theme.footer_key));
        spans.push(Span::styled(format!(" {label}"), theme.footer_label));
    }
    frame.render_widget(Line::from(spans), area);
}

/// Render the visible tail of a line buffer through the ANSI helper.
fn render_log_tail<'a>(
    frame: &mut Frame,
    area: Rect,
    lines: impl ExactSizeIterator<Item = &'a str>,
    scroll: &ScrollState,
) {
    let height = area.height as usize;
    let range = scroll.visible_range(height);
    let visible: Vec<Line> = lines
        .skip(range.start)
        .take(range.len())
        .map(ansi::to_line)
        .collect();
    frame.render_widget(Paragraph::new(visible), area);
}

/// The task screen: title, streamed log tail, close hint.
pub fn render_task(state: &TaskState, frame: &mut Frame, theme: &Theme) {
    let [header, log, footer] = screen_chrome(frame.area());
    render_header(frame, header, &state.title, "", theme);
    render_log_tail(
        frame,
        log,
        state.lines.iter().map(String::as_str),
        &state.scroll,
    );
    render_footer_hint(
        frame,
        footer,
        &[("esc", "back (terminates if running)")],
        theme,
    );
}

/// The attached-log screen: title, tailed log + liveness notices, detach
/// hint (an observer never terminates the run).
pub fn render_attached(state: &AttachedLogState, frame: &mut Frame, theme: &Theme) {
    let [header, log, footer] = screen_chrome(frame.area());
    render_header(frame, header, &state.title, "", theme);
    let range = state.scroll.visible_range(log.height as usize);
    let visible: Vec<Line> = state
        .lines
        .iter()
        .skip(range.start)
        .take(range.len())
        .map(|l| match l {
            LogLine::Raw(s) => ansi::to_line(s),
            LogLine::Notice { text, error } => {
                let style = if *error {
                    theme.status_error
                } else {
                    theme
                        .rich_style("bold green")
                        .expect("default theme maps green")
                };
                Line::from(Span::styled(text.clone(), style))
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(visible), log);
    render_footer_hint(frame, footer, &[("esc", "detach (run keeps going)")], theme);
}

/// The deploy screen's content area.
#[must_use]
pub fn deploy_content_area(area: Rect) -> Rect {
    deploy_areas(area)[3]
}

/// [header, build line, tab bar, content, recap strip, footer].
fn deploy_areas(area: Rect) -> [Rect; 6] {
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area)
}

/// One deploy tab's bar label (`<glyph> <name>` styled by state for hosts).
fn deploy_tab_label(view: &DeployViewState, tab: &DeployTab, theme: &Theme) -> Span<'static> {
    match tab {
        DeployTab::Build => Span::raw("⚙ build"),
        DeployTab::Playbook => Span::raw("ansible"),
        DeployTab::Summary => Span::raw("summary"),
        DeployTab::Host(name) => {
            let state = view
                .hosts
                .iter()
                .find(|h| h.name == *name)
                .map_or(HostState::Pending, |h| h.state);
            Span::styled(
                format!("{} {name}", host_state_glyph(state)),
                theme.host_state(state),
            )
        }
    }
}

/// The full deploy screen, including the pure native build-forest widget.
pub fn render_deploy(view: &DeployViewState, frame: &mut Frame, theme: &Theme) {
    let [header, build, tab_bar, content, recap, footer] = deploy_areas(frame.area());
    render_header(frame, header, "deploy runner", &view.sub_title(), theme);
    frame.render_widget(Line::from(view.build_line.clone()), build);

    // Tab bar: active reversed on top of the label's own style.
    let mut spans = Vec::new();
    for (i, tab) in deploy_tabs(view).into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("│"));
        }
        let mut label = deploy_tab_label(view, &tab, theme);
        label.content = format!(" {} ", label.content).into();
        if tab == view.active {
            label.style = label
                .style
                .add_modifier(Modifier::REVERSED | Modifier::BOLD);
        } else if !matches!(tab, DeployTab::Host(_)) {
            label.style = label.style.add_modifier(Modifier::DIM);
        }
        spans.push(label);
    }
    frame.render_widget(Line::from(spans), tab_bar);

    match &view.active {
        DeployTab::Build => {
            if let Some(snapshot) = &view.forest {
                frame.render_widget(
                    ForestWidget::new(snapshot)
                        .styles(theme.forest())
                        .scroll(view.build_scroll.top_offset(content.height as usize)),
                    content,
                );
            } else {
                frame.render_widget(
                    Paragraph::new("waiting for Nix build activity…").style(theme.footer_label),
                    content,
                );
            }
        }
        DeployTab::Playbook => {
            render_log_tail(
                frame,
                content,
                view.playbook_lines.iter().map(String::as_str),
                &view.playbook_scroll,
            );
        }
        DeployTab::Host(name) => {
            if let Some(host) = view.hosts.iter().find(|h| h.name == *name) {
                let scroll = view.host_scrolls.get(name).copied().unwrap_or_default();
                render_log_tail(
                    frame,
                    content,
                    host.lines.iter().map(String::as_str),
                    &scroll,
                );
            }
        }
        DeployTab::Summary => {
            if let Some(summary) = &view.summary {
                render_summary(summary, frame, content, theme);
            }
        }
    }

    // Recap strip: glyph name:state per host, waiting notice when none.
    let mut recap_spans = Vec::new();
    if view.hosts.is_empty() {
        recap_spans.push(Span::styled("waiting for host events…", theme.footer_label));
    }
    for host in &view.hosts {
        recap_spans.push(Span::styled(
            format!(
                " {} {}:{} ",
                host_state_glyph(host.state),
                host.name,
                host.state.as_str()
            ),
            theme.host_state(host.state),
        ));
    }
    frame.render_widget(Line::from(recap_spans), recap);

    let esc_hint = if view.attached {
        ("esc", "detach (run keeps going)")
    } else {
        ("esc", "back (terminates a running deploy)")
    };
    render_footer_hint(
        frame,
        footer,
        &[
            ("b", "build forest tab"),
            ("p", "playbook output tab"),
            ("s", "summary tab"),
            ("tab", "cycle tabs"),
            esc_hint,
        ],
        theme,
    );
}

/// The summary tab body: head + build line, host table, PLAY RECAP verbatim.
fn render_summary(summary: &SummaryState, frame: &mut Frame, area: Rect, theme: &Theme) {
    let head_style = if summary.ok {
        theme
            .rich_style("bold green")
            .expect("default theme maps green")
    } else {
        theme.status_error
    };
    let build_style = if summary.build_bad {
        theme.status_error
    } else {
        theme.footer_label
    };
    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled(summary.head.clone(), head_style),
            Span::styled(
                summary.meta.clone(),
                Style::new().add_modifier(Modifier::DIM),
            ),
        ]),
        Line::from(Span::styled(summary.build_line.clone(), build_style)),
        Line::default(),
        Line::from(Span::styled(
            format!("{:<24} {:<12} {:>4}", "host", "state", "rc"),
            Style::new().add_modifier(Modifier::BOLD),
        )),
    ];
    for (name, state, rc) in &summary.hosts {
        let style = theme.host_state(*state);
        lines.push(Line::from(Span::styled(
            format!(
                "{:<24} {:<12} {:>4}",
                format!("{} {name}", host_state_glyph(*state)),
                state.as_str(),
                rc.map_or_else(|| "-".to_string(), |v| v.to_string()),
            ),
            style,
        )));
    }
    if !summary.recap.is_empty() {
        lines.push(Line::default());
        lines.extend(summary.recap.iter().map(|l| ansi::to_line(l)));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exhaustiveness gate for the deploy tier's style/glyph tables:
    /// every host state maps through `rich_style` (a new state extends the
    /// match — compile-enforced — and must land here styleable).
    #[test]
    fn every_host_state_has_style_and_glyph() {
        for state in [
            HostState::Pending,
            HostState::Evaluating,
            HostState::Building,
            HostState::Copying,
            HostState::Activating,
            HostState::Waiting,
            HostState::Confirmed,
            HostState::RolledBack,
            HostState::Failed,
        ] {
            assert!(
                rich_style(host_state_style_spec(state)).is_some(),
                "unmapped spec for {state:?}"
            );
            assert!(!host_state_glyph(state).is_empty());
        }
    }

    #[test]
    fn state_tables_match_the_python_verbatim() {
        use ratatui::style::Color;
        // Spot the table values (deploy.py `_STATE_STYLE` / `_STATE_GLYPH`).
        assert_eq!(host_state_glyph(HostState::Confirmed), "✓");
        assert_eq!(host_state_glyph(HostState::RolledBack), "↩");
        assert_eq!(host_state_glyph(HostState::Copying), "⇄");
        let rolled = host_state_style(HostState::RolledBack);
        assert_eq!(rolled.fg, Some(Color::Red));
        assert!(rolled.add_modifier.contains(Modifier::BOLD));
        let copying = host_state_style(HostState::Copying);
        assert_eq!(copying.fg, Some(Color::Blue));
        let pending = host_state_style(HostState::Pending);
        assert!(pending.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn scrollback_is_capped() {
        let mut task = TaskState::new("t", 1, false);
        for i in 0..(SCROLLBACK_MAX + 10) {
            task.push_line(format!("line {i}"));
        }
        assert_eq!(task.lines.len(), SCROLLBACK_MAX);
        assert_eq!(task.lines.front().unwrap(), "line 10");
    }
}
