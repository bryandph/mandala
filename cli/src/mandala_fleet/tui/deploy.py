"""Deploy-runner view: the fan-out playbook as a subprocess, presented.

The playbook is the engine (limit guard, throttle, deploy-rs magic
rollback — never bypassed); this view launches it with the event
channel set and renders the per-host JSONL streams: a build pane fed by
the build plugin's progress events (the sanctioned fallback for a nom
pty), one tab per host color-coded by milestone state, and a recap
footer. A rolled-back host is flagged loudly; the rest of the fan-out
proceeds untouched.
"""

from __future__ import annotations

import re
import time

from rich.table import Table
from rich.text import Text
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import VerticalScroll
from textual.widgets import Footer, Header, RichLog, Static, TabbedContent, TabPane

from ..runner import DeployRun, HostState
from .nom import NomPane

# deploy-rs / nix progress output carries cursor-control CSI sequences
# (erase-line ESC[K, cursor moves) besides SGR colors. Keep the colors
# (Text.from_ansi understands SGR), drop everything else — rendered raw
# they shred the panes.
_CSI_RE = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]")
# C0 controls except ESC (SGR survives for from_ansi) and tab.
_CTRL_RE = re.compile(r"[\x00-\x08\x0b-\x1a\x1c-\x1f\x7f]")


def _to_text(line: str) -> Text:
    cleaned = _CSI_RE.sub(lambda m: m.group(0) if m.group(0).endswith("m") else "", line)
    cleaned = _CTRL_RE.sub("", cleaned)
    return Text.from_ansi(cleaned)

_STATE_STYLE = {
    HostState.PENDING: "dim",
    HostState.EVALUATING: "cyan",
    HostState.BUILDING: "cyan",
    HostState.COPYING: "blue",
    HostState.ACTIVATING: "yellow",
    HostState.WAITING: "yellow",
    HostState.CONFIRMED: "green",
    HostState.ROLLED_BACK: "bold red",
    HostState.FAILED: "bold red",
}

_STATE_GLYPH = {
    HostState.PENDING: "○",
    HostState.EVALUATING: "…",
    HostState.BUILDING: "⚙",
    HostState.COPYING: "⇄",
    HostState.ACTIVATING: "⚡",
    HostState.WAITING: "⏳",
    HostState.CONFIRMED: "✓",
    HostState.ROLLED_BACK: "↩",
    HostState.FAILED: "✗",
}


class DeployApp(App):
    TITLE = "mandala — deploy runner"
    CSS = """
    #build { height: 3; border: solid $surface; padding: 0 1; }
    #recap { dock: bottom; height: 1; padding: 0 1; }
    """
    BINDINGS = [
        Binding("q", "quit_run", "quit (terminates a running deploy)"),
        Binding("b", "build_tab", "nom build tab"),
        Binding("p", "playbook_log", "playbook output tab"),
        Binding("s", "summary_tab", "summary tab"),
    ]

    def __init__(self, run: DeployRun) -> None:
        super().__init__()
        self.run_model = run
        self._rendered: dict[str, int] = {}
        self._build_rendered = 0
        self._nom_finished = False
        self._summary_shown = False

    def compose(self) -> ComposeResult:
        yield Header(show_clock=True)
        yield Static(id="build")
        with TabbedContent(id="hosts", initial="tab-nom"):
            with TabPane("⚙ build", id="tab-nom"):
                yield NomPane(id="nom")
            with TabPane("ansible", id="tab-playbook"):
                yield RichLog(id="log-playbook", wrap=True, max_lines=4000)
        yield Static(id="recap")
        yield Footer()

    def on_mount(self) -> None:
        self.run_model.start()
        # Live-wire the internal-json stream into the nom tab — attached
        # before the first poll, so nom sees the build from line one.
        if self.run_model.tailer is not None:
            self.run_model.tailer.nixlog_sink = self.query_one("#nom", NomPane).feed
        self.sub_title = f"-l {self.run_model.limit}" + (
            " (dry-activate)" if self.run_model.dry_activate else ""
        )
        self.set_interval(0.25, self._tick)

    def _tick(self) -> None:
        run = self.run_model
        run.poll()
        if run.tailer.build.done and not self._nom_finished:
            self._nom_finished = True
            self.query_one("#nom", NomPane).finish()  # EOF -> nom's final summary
        self._render_build()
        self._render_playbook_output()
        for name in sorted(run.tailer.hosts):
            self._render_host(name)
        self._render_recap()
        if run.finished and not self._summary_shown:
            self._summary_shown = True
            self.sub_title += f" — exit {run.returncode}"
            self._show_summary()

    def _render_build(self) -> None:
        b = self.run_model.tailer.build
        head = (
            f"batch build  built {b.finished}/{b.built}  "
            f"fetched {b.fetched_done}/{b.fetched}  errors {b.errors}"
        )
        if b.done:
            head += f"  —  done rc={b.rc}"
        elif b.current:
            head += f"  —  {b.current}"
        # One-line summary; the nom tab carries the full tree.
        self.query_one("#build", Static).update(Text(head))

    def _render_playbook_output(self) -> None:
        log = self.query_one("#log-playbook", RichLog)
        lines = list(self.run_model.output)
        done = self._build_rendered
        for line in lines[done:]:
            log.write(_to_text(line))
        self._build_rendered = len(lines)

    def _render_host(self, name: str) -> None:
        host = self.run_model.tailer.hosts[name]
        tabs = self.query_one("#hosts", TabbedContent)
        pane_id = f"tab-{name}"
        if not tabs.query(f"#{pane_id}"):
            log = RichLog(id=f"log-{name}", wrap=True, max_lines=2000)
            tabs.add_pane(TabPane(name, log, id=pane_id))
            self._rendered[name] = 0
        log = self.query_one(f"#log-{name}", RichLog)
        lines = list(host.lines)
        for line in lines[self._rendered[name]:]:
            log.write(_to_text(line))
        self._rendered[name] = len(lines)
        tab = tabs.get_tab(pane_id)
        tab.label = Text(
            f"{_STATE_GLYPH[host.state]} {name}", style=_STATE_STYLE[host.state]
        )

    def _render_recap(self) -> None:
        hosts = self.run_model.tailer.hosts.values()
        recap = Text()
        if not hosts:
            recap.append("waiting for host events…", style="dim")
        for host in sorted(hosts, key=lambda h: h.name):
            recap.append(
                f" {_STATE_GLYPH[host.state]} {host.name}:{host.state.value} ",
                style=_STATE_STYLE[host.state],
            )
        self.query_one("#recap", Static).update(recap)

    def _show_summary(self) -> None:
        """Materialize + focus the summary tab once the playbook exits."""
        run = self.run_model
        rc = run.returncode
        elapsed = time.monotonic() - run.started_at if run.started_at else 0

        minutes, seconds = divmod(int(elapsed), 60)
        head = Text()
        head.append(
            f"deploy {'succeeded' if rc == 0 else f'FAILED (exit {rc})'}",
            style="bold green" if rc == 0 else "bold red",
        )
        head.append(
            f"   -l {run.limit}   {minutes}m{seconds:02d}s"
            + ("   dry-activate" if run.dry_activate else ""),
            style="dim",
        )

        b = run.tailer.build
        build_line = Text(
            f"batch build: built {b.finished}/{b.built}, fetched "
            f"{b.fetched_done}/{b.fetched}, errors {b.errors}, rc {b.rc}",
            style="red" if b.rc not in (0, None) else "dim",
        )

        table = Table(box=None, pad_edge=False, show_header=True, header_style="bold")
        table.add_column("host")
        table.add_column("state")
        table.add_column("rc", justify="right")
        for host in sorted(run.tailer.hosts.values(), key=lambda h: h.name):
            style = _STATE_STYLE[host.state]
            table.add_row(
                Text(f"{_STATE_GLYPH[host.state]} {host.name}", style=style),
                Text(host.state.value, style=style),
                Text("-" if host.rc is None else str(host.rc), style=style),
            )

        # ansible's own per-host accounting, verbatim.
        out = list(run.output)
        recap_at = next((i for i, l in enumerate(out) if "PLAY RECAP" in l), None)
        recap = Text()
        if recap_at is not None:
            for line in out[recap_at:]:
                recap.append_text(_to_text(line))
                recap.append("\n")

        body = Text("\n").join([head, build_line])

        tabs = self.query_one("#hosts", TabbedContent)
        tabs.add_pane(
            TabPane(
                # Plain str: this textual's TabPane title goes through
                # Content.from_markup, which rejects rich Text objects.
                "summary",
                VerticalScroll(
                    Static(body), Static(table), Static(recap), id="summary-scroll"
                ),
                id="tab-summary",
            )
        )
        tabs.active = "tab-summary"

    def action_summary_tab(self) -> None:
        if self.query("#tab-summary"):
            self.query_one("#hosts", TabbedContent).active = "tab-summary"

    def action_playbook_log(self) -> None:
        self.query_one("#hosts", TabbedContent).active = "tab-playbook"

    def action_build_tab(self) -> None:
        self.query_one("#hosts", TabbedContent).active = "tab-nom"

    def action_quit_run(self) -> None:
        self.run_model.terminate()
        self.exit(self.run_model.returncode)
