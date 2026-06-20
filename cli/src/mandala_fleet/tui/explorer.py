"""Fleet TUI: members / groups / drift tabs + the action tier.

The VIEWS are strictly read-only — browsing members, groups, and the
deployed-generation drift dashboard never mutates fleet state. Actions
(ping, reboot, deploy) target the cursor's selection and run in pushed
SCREENS, so the mutation boundary is a screen edge the operator crosses
deliberately; reboot and deploy additionally sit behind a confirm modal,
and the playbooks keep their own guards (--limit, k8s drain handling in
the operator's reboot playbook).

Task availability is conventional: ping is ansible ad-hoc; reboot is
offered only when the operator repo ships playbooks/reboot.yaml.
"""

from __future__ import annotations

import os
import subprocess
import time
from collections import deque
from pathlib import Path

from rich.text import Text
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Vertical
from textual.widgets import Footer, Header, Static, TabbedContent, TabPane

from .. import drift as drift_mod
from ..inventory import Inventory, surfaces
from ..runner import DeployRun
from .deploy import DeployScreen
from .select_table import SelectTable
from .tasks import ConfirmScreen, RebootScreen, TaskScreen

# Braille spinner frames for the background-job indicator (see the status
# machinery below). One shared frame animates every running job at once.
_SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"


def _ansible_dir() -> Path:
    return Path("ansible") if Path("ansible/ansible.cfg").is_file() else Path(".")


class ExplorerApp(App):
    """Fleet explorer: tabs to browse and select, keys to act."""

    TITLE = "mandala — fleet"
    CSS = """
    /* Fill the space between header and footer, and constrain the tables
       to it — an auto-height DataTable grows past the viewport and never
       scrolls. */
    #views { height: 1fr; }
    #members-table, #groups-table, #drift-table { height: 1fr; }
    #drift-hint { dock: bottom; height: 1; padding: 0 1; color: $text-muted; }
    """
    BINDINGS = [
        Binding("q", "quit", "quit"),
        Binding("r", "reload", "reload"),
        Binding("S", "refresh_drift", "refresh drift (survey + eval)"),
        Binding("p", "ping", "ping selection"),
        Binding("R", "reboot", "reboot selection"),
        Binding("D", "deploy", "deploy selection"),
    ]

    def __init__(self, inventory: Inventory) -> None:
        super().__init__()
        self.inventory = inventory
        self.expected: dict[str, str] | None = None
        self._rev: str | None = None
        self._cached_rev: str | None = None
        # Background-job state. `_busy` covers any nix eval in flight (the
        # inventory aggregate on load/reload, or the expected-toplevel eval);
        # `_surveying` the read-only state survey. Both can run at once.
        self._busy = False
        self._surveying = False
        self._survey_n = 0  # snapshots surveyed so far this run
        # Resting bar text shown when no job is running, and the spinner timer.
        self._status = ""
        self._status_sticky = False  # an error holds until the next refresh
        self._spin = 0
        self._spin_timer = None

    # -- status indicators ---------------------------------------------
    #
    # eval and survey run CONCURRENTLY (action_refresh_drift fires both) and
    # the survey usually finishes first. A single status string meant the
    # survey's "drift refreshed" stomped the still-running eval, so you
    # couldn't tell the eval was in flight until the columns moved. Instead
    # each job owns a running flag; while any are set a timer animates one
    # spinner and the bar lists every job still running — the eval indicator
    # stays put after the faster survey lands. With all jobs idle the bar
    # shows the latest resting message (a result, or a sticky error).

    def _set_status(self, msg: str, *, error: bool = False) -> None:
        """Set the resting bar message (shown when nothing is running).

        Errors are sticky: a concurrently-finishing success will not
        overwrite them, so an eval failure survives the survey's "drift
        refreshed". The stickiness clears when the next refresh begins."""
        if error or not self._status_sticky:
            self._status = msg
            self._status_sticky = error
        self._render_status()

    def _render_status(self) -> None:
        jobs: list[str] = []
        if self._busy:
            jobs.append("eval")
        if self._surveying:
            jobs.append(f"survey ({self._survey_n} read)" if self._survey_n else "survey")
        if jobs:
            frame = _SPINNER[self._spin % len(_SPINNER)]
            self.sub_title = "running   " + "   ·   ".join(f"{frame} {j}" for j in jobs)
            self._ensure_spinner()
        else:
            self.sub_title = self._status

    def _ensure_spinner(self) -> None:
        if self._spin_timer is None:
            self._spin_timer = self.set_interval(0.1, self._tick)

    def _tick(self) -> None:
        self._spin += 1
        self._render_status()
        # Stop animating once every job is idle — the resting message needs
        # no ticking, and a live timer would spin forever.
        if not (self._busy or self._surveying) and self._spin_timer is not None:
            self._spin_timer.stop()
            self._spin_timer = None

    # -- layout --------------------------------------------------------

    def compose(self) -> ComposeResult:
        yield Header(show_clock=True)
        with TabbedContent(initial="tab-members", id="views"):
            with TabPane("members", id="tab-members"):
                yield SelectTable(id="members-table", zebra_stripes=True, cursor_type="row")
            with TabPane("groups", id="tab-groups"):
                yield SelectTable(id="groups-table", zebra_stripes=True, cursor_type="row")
            with TabPane("drift", id="tab-drift"):
                yield Vertical(
                    SelectTable(id="drift-table", zebra_stripes=True, cursor_type="row"),
                    Static(id="drift-hint"),
                )
        yield Footer()

    def on_mount(self) -> None:
        members = self.query_one("#members-table", SelectTable)
        members.add_columns("", "member", "platform", "arch", "category", "role", "tags", "ads")
        groups = self.query_one("#groups-table", SelectTable)
        groups.add_columns("", "group", "n", "members")
        drift = self.query_one("#drift-table", SelectTable)
        drift.add_columns("", "member", "status", "current", "expected", "booted", "captured")
        self._load()

    # -- data ----------------------------------------------------------

    def _load(self) -> None:
        """Aggregate eval OFF the UI thread — it takes tens of seconds on
        a real fleet, and blocking on_mount means a gray void instead of
        a first paint."""
        if self._busy:
            return
        self._busy = True
        self._render_status()  # shows the eval spinner
        inv = self.inventory

        def work() -> None:
            try:
                inv.aggregate  # force the cached_property
            except Exception as e:  # noqa: BLE001 — surfaced in the UI
                self.call_from_thread(self._load_failed, str(e))
                return
            # Reuse the rev-keyed expected cache when the contract hasn't
            # moved since the last eval (a mismatch is itself the signal).
            self._rev = drift_mod.repo_rev(inv.flake)
            self._cached_rev, cached = drift_mod.load_expected()
            if drift_mod.cache_fresh(self._cached_rev, self._rev):
                self.expected = cached
            self.call_from_thread(self._fill)

        self.run_worker(work, thread=True, exclusive=True)

    def _load_failed(self, error: str) -> None:
        self._busy = False
        self._set_status(
            f"aggregate eval failed: {error.splitlines()[-1] if error else 'unknown'}",
            error=True,
        )

    def _fill(self) -> None:
        inv = self.inventory

        members = self.query_one("#members-table", SelectTable)
        members.reset_rows()
        for name in sorted(inv.members):
            m = inv.members[name]
            members.add_named_row(
                name,
                name,
                m.get("platform", "?"),
                m.get("architecture", "?"),
                m.get("category", "?"),
                m.get("role") or "-",
                " ".join(m.get("tags", [])),
                surfaces(m),
            )

        groups = self.query_one("#groups-table", SelectTable)
        groups.reset_rows()
        for group, names in sorted(inv.groups.items()):
            groups.add_named_row(group, group, str(len(names)), " ".join(sorted(names)))

        self._fill_drift()
        self._busy = False
        self._set_status(
            f"{len(inv.members)} members, {len(inv.groups)} groups"
            " — space/shift+↑↓ select · p ping · R reboot · D deploy"
        )

    @property
    def _deploy_nodes(self) -> list[str]:
        deploy = self.inventory.aggregate.get("projections", {}).get("deploy", {})
        return list(deploy.get("nodes", []))

    def _fill_drift(self) -> None:
        snapshots = drift_mod.read_snapshots()
        entries = drift_mod.compare(self._deploy_nodes, snapshots, self.expected)
        table = self.query_one("#drift-table", SelectTable)
        table.reset_rows()
        short = lambda p: (p or "").removeprefix("/nix/store/")[:18]
        for e in entries:
            table.add_named_row(
                e.host,
                e.host,
                Text(e.status.value, style=drift_mod.STATUS_STYLE[e.status]),
                short(e.current),
                short(e.expected),
                short(e.booted),
                (e.captured_at or "")[:19],
            )
        hint = "S refresh drift (survey + eval) · R reboot a reboot-pending row"
        if self.expected is not None:
            hint += f"   expected @ {drift_mod.short_rev(self._rev)}"
        elif self._cached_rev is not None:
            hint += (
                f"   contract MOVED since last eval"
                f" (cache @ {drift_mod.short_rev(self._cached_rev)},"
                f" repo @ {drift_mod.short_rev(self._rev)}) — press S"
            )
        else:
            hint += "   (expected NOT evaluated yet — press S)"
        self.query_one("#drift-hint", Static).update(hint)

    # -- selection -----------------------------------------------------

    def _target(self) -> str | None:
        """The action target on the active tab: the multi-selection when
        one exists, else the cursor row. Names are members or groups (==
        ansible groups, one taxonomy one spelling), comma-joined into an
        ansible --limit."""
        active = self.query_one("#views", TabbedContent).active
        table_id = {
            "tab-members": "#members-table",
            "tab-groups": "#groups-table",
            "tab-drift": "#drift-table",
        }.get(active)
        if table_id is None:
            return None
        table = self.query_one(table_id, SelectTable)
        selected = table.selected_names
        if selected:
            return ",".join(selected)
        return table.cursor_name

    # -- actions (pushed screens; views stay read-only) -----------------

    def action_ping(self) -> None:
        target = self._target()
        if target is None:
            return
        self.push_screen(TaskScreen(
            f"ping {target}",
            ["ansible", target, "-m", "ping"],
            _ansible_dir(),
            # Deliberately the DEFAULT stdout callback: --one-line AND the
            # oneline/minimal callbacks are deprecated in core 2.19
            # (removed 2.23) with no core replacement (ansible/ansible
            # #85333, closed not-planned), and community presentation
            # plugins would be a new dependency. The default callback is
            # the only stable surface; the pane wraps and scrolls.
        ))

    def action_reboot(self) -> None:
        target = self._target()
        if target is None:
            return
        # Prefer the operator's wrapper: it carries the controller-side env
        # raw ansible-playbook lacks — the delegated k8s drain pins
        # ANSIBLE_LOCAL_PYTHON_INTERPRETER to a python WITH the kubernetes
        # lib (the global interpreter default does not win for delegated
        # tasks, so without the wrapper the drain fails "kubernetes
        # library is missing").
        import shutil

        if shutil.which("ans-reboot"):
            base = ["ans-reboot", "-l", target]
        elif (_ansible_dir() / "playbooks/reboot.yaml").is_file():
            base = ["ansible-playbook", "playbooks/reboot.yaml", "-l", target]
        else:
            self._set_status("no ans-reboot wrapper or playbooks/reboot.yaml — reboot task unavailable")
            return

        # RebootScreen returns the chosen batch order + drain safety; both
        # ride through the wrapper as extra-vars (ans-reboot/ansible-playbook
        # both forward "$@"). reboot_serial drives the play's `serial`,
        # drain gates the k8s cordon/drain steps.
        def go(opts: dict | None) -> None:
            if not opts:
                return
            argv = base + [
                "-e", f"reboot_serial={opts['serial']}",
                "-e", f"drain={'true' if opts['drain'] else 'false'}",
            ]
            self.push_screen(
                TaskScreen(f"reboot {target}", argv, _ansible_dir()),
                self._after_mutation,
            )

        self.push_screen(RebootScreen(target), go)

    def action_deploy(self) -> None:
        target = self._target()
        if target is None:
            return

        def go(confirmed: bool | None) -> None:
            if confirmed:
                self.push_screen(
                    DeployScreen(DeployRun(limit=target)),
                    self._after_mutation,
                )

        self.push_screen(
            ConfirmScreen(f"Deploy '{target}'?\n(eval-once batch build, then deploy-rs per host with magic rollback)"),
            go,
        )

    # -- maintenance ---------------------------------------------------

    def action_reload(self) -> None:
        self.inventory = Inventory(flake=self.inventory.flake)
        self.expected = None
        self._load()

    def action_refresh_drift(self) -> None:
        """Refresh both drift inputs at once: the expected-toplevel nix
        eval (a background worker) and the read-only state survey (a
        pushed TaskScreen) run CONCURRENTLY, not as two separate
        keystrokes. Each refreshes the drift table as it lands; either
        completion order converges to the same judgement. Also fired
        automatically once a deploy or reboot completes (see
        `_after_mutation`)."""
        self._status_sticky = False  # a fresh refresh clears a stale error
        self.action_eval_expected()  # background worker, returns at once
        self.action_survey()  # pushed TaskScreen, runs alongside the eval

    def _after_mutation(self, rc: int | None) -> None:
        """A deploy/reboot screen just closed. If the run actually ran to
        completion (rc is set — an operator cancel pops with rc None),
        auto-refresh drift so the post-change state is surveyed without
        having to press S. A non-zero rc (a failed/partial run) still
        refreshes — seeing the resulting state is exactly what you want."""
        if rc is not None:
            self.action_refresh_drift()

    def action_eval_expected(self) -> None:
        if self._busy:
            return
        self._busy = True
        self._render_status()  # shows the eval spinner alongside any survey
        nodes = self._deploy_nodes
        flake = self.inventory.flake

        def work() -> None:
            try:
                expected = drift_mod.eval_expected(flake, nodes)
            except Exception as e:  # noqa: BLE001 — surfaced, not raised
                self.call_from_thread(self._drift_done, None, f"eval failed: {e}")
                return
            self._rev = drift_mod.repo_rev(flake)
            self._cached_rev = self._rev
            drift_mod.save_expected(self._rev, expected)
            self.call_from_thread(self._drift_done, expected, None)

        self.run_worker(work, thread=True, exclusive=True)

    def action_survey(self) -> None:
        """Read-only state survey, run in the BACKGROUND rather than as a
        screen. `ansible-playbook mandala.fleet.state` writes one
        <host>.json snapshot per host into the state dir; we count the
        snapshots freshly written THIS run and report the running tally in
        the top bar, then refresh drift when it finishes. Output is
        captured (writing it to the terminal would corrupt the TUI) and
        surfaced only on failure. Its own worker group so it runs
        alongside the expected-toplevel eval instead of cancelling it."""
        if self._surveying:
            return
        self._surveying = True
        self._survey_n = 0
        self._render_status()  # shows the survey spinner alongside any eval
        directory = drift_mod.state_dir()
        cwd = _ansible_dir()
        env = dict(
            os.environ,
            MANDALA_FLEET_STATE=str(directory),
            PYTHONUNBUFFERED="1",
            ANSIBLE_FORCE_COLOR="0",
        )
        argv = ["ansible-playbook", "mandala.fleet.state"]
        # -1s so a snapshot written in the same clock second as launch
        # still counts as "this run".
        started = time.time() - 1

        def fresh() -> int:
            # Per-host snapshots are <host>.json; skip dotfiles so the
            # .expected.json eval cache (rewritten in this same dir by the
            # concurrent eval worker) is never miscounted as a host.
            if not directory.is_dir():
                return 0
            n = 0
            for p in directory.glob("*.json"):
                if p.name.startswith("."):
                    continue
                try:
                    if p.stat().st_mtime >= started:
                        n += 1
                except OSError:
                    pass
            return n

        def work() -> None:
            out: deque[str] = deque(maxlen=2000)
            try:
                proc = subprocess.Popen(
                    argv,
                    cwd=cwd,
                    env=env,
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT,
                    text=True,
                )
            except OSError as e:
                self.call_from_thread(self._survey_done, 0, 1, f"failed to launch: {e}")
                return
            assert proc.stdout is not None
            last = -1
            # Draining stdout (even just to discard) avoids a full-pipe
            # deadlock; each line is also a cheap cue to recount.
            for line in proc.stdout:
                out.append(line.rstrip("\n"))
                n = fresh()
                if n != last:
                    last = n
                    self.call_from_thread(self._survey_progress, n)
            rc = proc.wait()
            err = None if rc == 0 else (out[-1] if out else None)
            self.call_from_thread(self._survey_done, fresh(), rc, err)

        self.run_worker(work, thread=True, exclusive=True, group="survey")

    def _survey_progress(self, n: int) -> None:
        self._survey_n = n
        self._render_status()  # live tally rides the spinner line

    def _survey_done(self, n: int, rc: int, error: str | None = None) -> None:
        self._surveying = False
        self._survey_n = n
        self._fill_drift()
        if rc == 0:
            self._set_status(f"drift refreshed · surveyed {n} host{'' if n == 1 else 's'}")
        else:
            self._set_status(f"survey failed (exit {rc}): {error or ''}".rstrip(), error=True)

    def _drift_done(self, expected: dict[str, str] | None, error: str | None) -> None:
        self._busy = False
        self.expected = expected
        self._fill_drift()
        # _render_status keeps the survey spinner up if it is still counting,
        # so this resting message only surfaces once both jobs are idle; an
        # eval error is sticky and wins over the survey's success message.
        self._set_status(error or "drift refreshed", error=error is not None)
