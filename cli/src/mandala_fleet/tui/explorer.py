"""Read-only tier: fleet explorer + deployed-generation drift dashboard.

These views READ — the inventory aggregate, controller-side state
snapshots, a local nix eval — and never issue a deploy, push, or write.
The only subprocesses they spawn are the read-only state survey
(mandala.fleet.state, a fact-gather) and `nix eval`.
"""

from __future__ import annotations

from pathlib import Path

from rich.text import Text
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal
from textual.screen import Screen
from textual.widgets import DataTable, Footer, Header, Tree

from .. import drift as drift_mod
from ..inventory import Inventory, surfaces

_DRIFT_STYLE = {
    drift_mod.DriftStatus.IN_SYNC: "green",
    drift_mod.DriftStatus.DRIFT: "bold red",
    drift_mod.DriftStatus.REBOOT_PENDING: "yellow",
    drift_mod.DriftStatus.NO_SNAPSHOT: "dim",
    drift_mod.DriftStatus.UNREACHABLE: "magenta",
}


class DriftScreen(Screen):
    """Deployed-generation drift over the deploy-rs members."""

    BINDINGS = [
        Binding("escape", "app.pop_screen", "back"),
        Binding("r", "reload", "re-read snapshots"),
        Binding("e", "eval_expected", "eval expected (slow)"),
        Binding("s", "survey", "run state survey"),
    ]

    def __init__(self, inventory: Inventory) -> None:
        super().__init__()
        self.inventory = inventory
        self.expected: dict[str, str] | None = None
        self._busy = False

    def compose(self) -> ComposeResult:
        yield Header(show_clock=True)
        yield DataTable(zebra_stripes=True, cursor_type="row")
        yield Footer()

    def on_mount(self) -> None:
        table = self.query_one(DataTable)
        table.add_columns("member", "status", "current", "expected", "booted", "captured")
        self.action_reload()

    @property
    def _deploy_nodes(self) -> list[str]:
        deploy = self.inventory.aggregate.get("projections", {}).get("deploy", {})
        return list(deploy.get("nodes", []))

    def action_reload(self) -> None:
        snapshots = drift_mod.read_snapshots()
        entries = drift_mod.compare(self._deploy_nodes, snapshots, self.expected)
        table = self.query_one(DataTable)
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
        self.sub_title = f"{len(entries)} members" + (
            "" if self.expected else " — expected not evaluated (press e)"
        )

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
                self.app.call_from_thread(self._done, None, f"eval failed: {e}")
                return
            self.app.call_from_thread(self._done, expected, None)

        self.run_worker(work, thread=True, exclusive=True)

    def action_survey(self) -> None:
        if self._busy:
            return
        self._busy = True
        self.sub_title = "running mandala.fleet.state (read-only fact gather)…"
        ansible_dir = Path("ansible") if Path("ansible/ansible.cfg").is_file() else Path(".")

        def work() -> None:
            rc = drift_mod.refresh_snapshots(ansible_dir)
            self.app.call_from_thread(
                self._done, self.expected, None if rc == 0 else f"survey rc={rc}"
            )

        self.run_worker(work, thread=True, exclusive=True)

    def _done(self, expected: dict[str, str] | None, error: str | None) -> None:
        self._busy = False
        self.expected = expected
        self.action_reload()
        if error:
            self.sub_title = error


class ExplorerApp(App):
    """Fleet explorer: members table + group tree, strictly read-only."""

    TITLE = "mandala — fleet explorer"
    CSS = """
    Tree { width: 32; dock: left; border-right: solid $surface; }
    """
    BINDINGS = [
        Binding("q", "quit", "quit"),
        Binding("d", "drift", "drift dashboard"),
        Binding("r", "reload", "reload inventory"),
    ]

    def __init__(self, inventory: Inventory) -> None:
        super().__init__()
        self.inventory = inventory
        self._group: str | None = None

    def compose(self) -> ComposeResult:
        yield Header(show_clock=True)
        with Horizontal():
            yield Tree("groups")
            yield DataTable(zebra_stripes=True, cursor_type="row")
        yield Footer()

    def on_mount(self) -> None:
        table = self.query_one(DataTable)
        table.add_columns("member", "platform", "arch", "category", "role", "tags", "ads")
        self._load()

    def _load(self) -> None:
        """Evaluate the aggregate OFF the UI thread — `nix eval .#mandala`
        takes tens of seconds on a real fleet, and blocking on_mount means
        a gray void instead of a first paint."""
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
        tree = self.query_one(Tree)
        tree.clear()
        tree.root.expand()
        all_node = tree.root.add_leaf("all members", data=None)
        for group, names in sorted(inv.groups.items()):
            tree.root.add_leaf(f"{group} ({len(names)})", data=group)
        tree.select_node(all_node)
        self._render_table()

    def _render_table(self) -> None:
        inv = self.inventory
        table = self.query_one(DataTable)
        table.clear()
        names = inv.groups.get(self._group, []) if self._group else inv.members.keys()
        for name in sorted(names):
            m = inv.members[name]
            table.add_row(
                name,
                m.get("platform", "?"),
                m.get("architecture", "?"),
                m.get("category", "?"),
                m.get("role") or "-",
                " ".join(m.get("tags", [])),
                surfaces(m),
            )
        scope = self._group or "all"
        self.sub_title = f"{table.row_count} members — {scope} (ads = ansible/deploy/sops)"

    def on_tree_node_selected(self, event: Tree.NodeSelected) -> None:
        self._group = event.node.data
        self._render_table()

    def action_drift(self) -> None:
        self.push_screen(DriftScreen(self.inventory))

    def action_reload(self) -> None:
        self.inventory = Inventory(flake=self.inventory.flake)
        self._load()
