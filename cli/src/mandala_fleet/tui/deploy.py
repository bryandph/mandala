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

from rich.text import Text
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.widgets import Footer, Header, RichLog, Static, TabbedContent, TabPane

from ..runner import DeployRun, HostState

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
    #build { height: 9; border: solid $surface; padding: 0 1; }
    #recap { dock: bottom; height: 1; padding: 0 1; }
    """
    BINDINGS = [
        Binding("q", "quit_run", "quit (terminates a running deploy)"),
        Binding("p", "playbook_log", "playbook output tab"),
    ]

    def __init__(self, run: DeployRun) -> None:
        super().__init__()
        self.run_model = run
        self._rendered: dict[str, int] = {}
        self._build_rendered = 0

    def compose(self) -> ComposeResult:
        yield Header(show_clock=True)
        yield Static(id="build")
        with TabbedContent(id="hosts"):
            with TabPane("ansible", id="tab-playbook"):
                yield RichLog(id="log-playbook", wrap=False, max_lines=4000)
        yield Static(id="recap")
        yield Footer()

    def on_mount(self) -> None:
        self.run_model.start()
        self.sub_title = f"-l {self.run_model.limit}" + (
            " (dry-activate)" if self.run_model.dry_activate else ""
        )
        self.set_interval(0.25, self._tick)

    def _tick(self) -> None:
        run = self.run_model
        run.poll()
        self._render_build()
        self._render_playbook_output()
        for name in sorted(run.tailer.hosts):
            self._render_host(name)
        self._render_recap()
        if run.finished and self.sub_title and "exit" not in self.sub_title:
            self.sub_title += f" — exit {run.returncode}"

    def _render_build(self) -> None:
        b = self.run_model.tailer.build
        head = (
            f"batch build  built {b.finished}/{b.built}  "
            f"fetched {b.fetched_done}/{b.fetched}  errors {b.errors}"
        )
        if b.current:
            head += f"\ncurrent: {b.current}"
        if b.done:
            head += f"\ndone rc={b.rc}"
        tail = "\n".join(list(b.lines)[-5:])
        self.query_one("#build", Static).update(head + ("\n" + tail if tail else ""))

    def _render_playbook_output(self) -> None:
        log = self.query_one("#log-playbook", RichLog)
        lines = list(self.run_model.output)
        done = self._build_rendered
        for line in lines[done:]:
            log.write(line)
        self._build_rendered = len(lines)

    def _render_host(self, name: str) -> None:
        host = self.run_model.tailer.hosts[name]
        tabs = self.query_one("#hosts", TabbedContent)
        pane_id = f"tab-{name}"
        if not tabs.query(f"#{pane_id}"):
            log = RichLog(id=f"log-{name}", wrap=False, max_lines=2000)
            tabs.add_pane(TabPane(name, log, id=pane_id))
            self._rendered[name] = 0
        log = self.query_one(f"#log-{name}", RichLog)
        lines = list(host.lines)
        for line in lines[self._rendered[name]:]:
            log.write(line)
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

    def action_playbook_log(self) -> None:
        self.query_one("#hosts", TabbedContent).active = "tab-playbook"

    def action_quit_run(self) -> None:
        self.run_model.terminate()
        self.exit(self.run_model.returncode)
