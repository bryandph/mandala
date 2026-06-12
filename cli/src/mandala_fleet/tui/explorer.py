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

from pathlib import Path

from rich.text import Text
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Vertical
from textual.widgets import DataTable, Footer, Header, Static, TabbedContent, TabPane

from .. import drift as drift_mod
from ..inventory import Inventory, surfaces
from ..runner import DeployRun
from .deploy import DeployScreen
from .tasks import ConfirmScreen, TaskScreen

_DRIFT_STYLE = {
    drift_mod.DriftStatus.IN_SYNC: "green",
    drift_mod.DriftStatus.DRIFT: "bold red",
    drift_mod.DriftStatus.REBOOT_PENDING: "yellow",
    drift_mod.DriftStatus.NO_SNAPSHOT: "dim",
    drift_mod.DriftStatus.UNREACHABLE: "magenta",
}


def _ansible_dir() -> Path:
    return Path("ansible") if Path("ansible/ansible.cfg").is_file() else Path(".")


class ExplorerApp(App):
    """Fleet explorer: tabs to browse and select, keys to act."""

    TITLE = "mandala — fleet"
    CSS = """
    #drift-hint { dock: bottom; height: 1; padding: 0 1; color: $text-muted; }
    """
    BINDINGS = [
        Binding("q", "quit", "quit"),
        Binding("r", "reload", "reload"),
        Binding("e", "eval_expected", "eval expected (drift, slow)"),
        Binding("S", "survey", "state survey (read-only)"),
        Binding("p", "ping", "ping selection"),
        Binding("R", "reboot", "reboot selection"),
        Binding("D", "deploy", "deploy selection"),
    ]

    def __init__(self, inventory: Inventory) -> None:
        super().__init__()
        self.inventory = inventory
        self.expected: dict[str, str] | None = None
        self._busy = False

    # -- layout --------------------------------------------------------

    def compose(self) -> ComposeResult:
        yield Header(show_clock=True)
        with TabbedContent(initial="tab-members", id="views"):
            with TabPane("members", id="tab-members"):
                yield DataTable(id="members-table", zebra_stripes=True, cursor_type="row")
            with TabPane("groups", id="tab-groups"):
                yield DataTable(id="groups-table", zebra_stripes=True, cursor_type="row")
            with TabPane("drift", id="tab-drift"):
                yield Vertical(
                    DataTable(id="drift-table", zebra_stripes=True, cursor_type="row"),
                    Static(id="drift-hint"),
                )
        yield Footer()

    def on_mount(self) -> None:
        members = self.query_one("#members-table", DataTable)
        members.add_columns("member", "platform", "arch", "category", "role", "tags", "ads")
        groups = self.query_one("#groups-table", DataTable)
        groups.add_columns("group", "n", "members")
        drift = self.query_one("#drift-table", DataTable)
        drift.add_columns("member", "status", "current", "expected", "booted", "captured")
        self._load()

    # -- data ----------------------------------------------------------

    def _load(self) -> None:
        """Aggregate eval OFF the UI thread — it takes tens of seconds on
        a real fleet, and blocking on_mount means a gray void instead of
        a first paint."""
        self.sub_title = f"evaluating {self.inventory.flake}#mandala…"
        inv = self.inventory

        def work() -> None:
            try:
                inv.aggregate  # force the cached_property
            except Exception as e:  # noqa: BLE001 — surfaced in the UI
                self.call_from_thread(self._load_failed, str(e))
                return
            self.call_from_thread(self._fill)

        self.run_worker(work, thread=True, exclusive=True)

    def _load_failed(self, error: str) -> None:
        self.sub_title = f"aggregate eval failed: {error.splitlines()[-1] if error else 'unknown'}"

    def _fill(self) -> None:
        inv = self.inventory

        members = self.query_one("#members-table", DataTable)
        members.clear()
        for name in sorted(inv.members):
            m = inv.members[name]
            members.add_row(
                name,
                m.get("platform", "?"),
                m.get("architecture", "?"),
                m.get("category", "?"),
                m.get("role") or "-",
                " ".join(m.get("tags", [])),
                surfaces(m),
            )

        groups = self.query_one("#groups-table", DataTable)
        groups.clear()
        for group, names in sorted(inv.groups.items()):
            groups.add_row(group, str(len(names)), " ".join(sorted(names)))

        self._fill_drift()
        self.sub_title = (
            f"{len(inv.members)} members, {len(inv.groups)} groups"
            " — p ping · R reboot · D deploy on the selected row"
        )

    @property
    def _deploy_nodes(self) -> list[str]:
        deploy = self.inventory.aggregate.get("projections", {}).get("deploy", {})
        return list(deploy.get("nodes", []))

    def _fill_drift(self) -> None:
        snapshots = drift_mod.read_snapshots()
        entries = drift_mod.compare(self._deploy_nodes, snapshots, self.expected)
        table = self.query_one("#drift-table", DataTable)
        table.clear()
        short = lambda p: (p or "").removeprefix("/nix/store/")[:18]
        for e in entries:
            table.add_row(
                e.host,
                Text(e.status.value, style=_DRIFT_STYLE[e.status]),
                short(e.current),
                short(e.expected),
                short(e.booted),
                (e.captured_at or "")[:19],
            )
        hint = "S re-survey · e eval expected · R reboot a reboot-pending row"
        if self.expected is None:
            hint += "   (expected NOT evaluated yet)"
        self.query_one("#drift-hint", Static).update(hint)

    # -- selection -----------------------------------------------------

    def _target(self) -> str | None:
        """The cursor's selection on the active tab: a member name, or a
        group name (== the ansible group, one taxonomy one spelling)."""
        active = self.query_one("#views", TabbedContent).active
        table_id = {
            "tab-members": "#members-table",
            "tab-groups": "#groups-table",
            "tab-drift": "#drift-table",
        }.get(active)
        if table_id is None:
            return None
        table = self.query_one(table_id, DataTable)
        if table.row_count == 0:
            return None
        value = table.get_cell_at((table.cursor_row, 0))
        return str(value.plain if isinstance(value, Text) else value)

    # -- actions (pushed screens; views stay read-only) -----------------

    def action_ping(self) -> None:
        target = self._target()
        if target is None:
            return
        self.push_screen(TaskScreen(
            f"ping {target}",
            ["ansible", target, "-m", "ping", "--one-line"],
            _ansible_dir(),
        ))

    def action_reboot(self) -> None:
        target = self._target()
        if target is None:
            return
        playbook = _ansible_dir() / "playbooks/reboot.yaml"
        if not playbook.is_file():
            self.sub_title = "no playbooks/reboot.yaml in this repo — reboot task unavailable"
            return

        def go(confirmed: bool | None) -> None:
            if confirmed:
                self.push_screen(TaskScreen(
                    f"reboot {target}",
                    ["ansible-playbook", "playbooks/reboot.yaml", "-l", target],
                    _ansible_dir(),
                ))

        self.push_screen(
            ConfirmScreen(f"Reboot '{target}'?\n(rolling, drain-aware — the playbook handles k8s nodes)"),
            go,
        )

    def action_deploy(self) -> None:
        target = self._target()
        if target is None:
            return

        def go(confirmed: bool | None) -> None:
            if confirmed:
                self.push_screen(DeployScreen(DeployRun(limit=target)))

        self.push_screen(
            ConfirmScreen(f"Deploy '{target}'?\n(eval-once batch build, then deploy-rs per host with magic rollback)"),
            go,
        )

    # -- maintenance ---------------------------------------------------

    def action_reload(self) -> None:
        self.inventory = Inventory(flake=self.inventory.flake)
        self.expected = None
        self._load()

    def action_eval_expected(self) -> None:
        if self._busy:
            return
        self._busy = True
        self.sub_title = "evaluating expected toplevels (one nix eval)…"
        nodes = self._deploy_nodes
        flake = self.inventory.flake

        def work() -> None:
            try:
                expected = drift_mod.eval_expected(flake, nodes)
            except Exception as e:  # noqa: BLE001 — surfaced, not raised
                self.call_from_thread(self._drift_done, None, f"eval failed: {e}")
                return
            self.call_from_thread(self._drift_done, expected, None)

        self.run_worker(work, thread=True, exclusive=True)

    def action_survey(self) -> None:
        """The read-only state survey, as a captured TaskScreen — running
        it raw lets ansible write straight to the terminal under the TUI."""

        def done(_rc: int | None) -> None:
            self._fill_drift()
            self.sub_title = "drift refreshed from new snapshots"

        self.push_screen(
            TaskScreen(
                "state survey (read-only fact gather)",
                ["ansible-playbook", "mandala.fleet.state"],
                _ansible_dir(),
                env={"MANDALA_FLEET_STATE": str(drift_mod.DEFAULT_STATE_DIR)},
            ),
            done,
        )

    def _drift_done(self, expected: dict[str, str] | None, error: str | None) -> None:
        self._busy = False
        self.expected = expected
        self._fill_drift()
        self.sub_title = error or "drift refreshed"
