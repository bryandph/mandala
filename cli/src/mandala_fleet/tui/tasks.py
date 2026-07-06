"""The action tier's task screens.

TaskScreen runs ONE subprocess (an ansible ad-hoc task or playbook) and
streams its output live; ConfirmScreen is the modal gate in front of
anything destructive. The read-only explorer views never run these
inline — they push screens, so the mutation boundary is a screen edge
the operator crosses deliberately.
"""

from __future__ import annotations

import os
import subprocess
import threading
from collections import deque
from pathlib import Path

from rich.text import Text
from textual.app import ComposeResult
from textual.binding import Binding
from textual.containers import Vertical
from textual.screen import ModalScreen, Screen
from textual.widgets import Footer, Header, RichLog, Static

from .render import to_text


class ConfirmScreen(ModalScreen[bool]):
    """y / escape modal for destructive actions."""

    CSS = """
    ConfirmScreen { align: center middle; }
    #confirm-box {
        width: 70; max-width: 90%; height: auto; padding: 1 2;
        border: thick $error; background: $surface;
    }
    """
    BINDINGS = [
        Binding("y", "confirm", "yes, run it"),
        Binding("escape,n", "cancel", "cancel"),
    ]

    def __init__(self, message: str) -> None:
        super().__init__()
        self._message = message

    def compose(self) -> ComposeResult:
        text = Text()
        text.append(self._message + "\n\n", style="bold")
        text.append("y", style="bold red")
        text.append(" to run   ")
        text.append("esc", style="bold")
        text.append(" to cancel")
        yield Vertical(Static(text), id="confirm-box")

    def action_confirm(self) -> None:
        self.dismiss(True)

    def action_cancel(self) -> None:
        self.dismiss(False)


class RebootScreen(ModalScreen[dict | None]):
    """Reboot options gate: pick batch order + k8s drain safety.

    Keyboard-driven like ConfirmScreen (no focusable form widgets, so it
    can't trap arrow/enter the way a RadioSet would) — number keys pick
    the order, `d` toggles drain, `y` runs. Dismisses with
    `{"serial": <str>, "drain": <bool>}` or None on cancel; the caller
    turns that into `-e reboot_serial=… -e drain=…` for the playbook.
    """

    # (key, label, playbook `serial` value). serial: 1 one-at-a-time,
    # 2 rolling, "100%" every targeted host at once (0 is rejected by
    # modern ansible, 100% is the portable "all in one batch").
    _ORDERS = [
        ("1", "Serial — one host at a time", "1"),
        ("2", "Rolling — 2 hosts in flight", "2"),
        ("3", "All-at-once — every targeted host together", "100%"),
    ]

    CSS = """
    RebootScreen { align: center middle; }
    #reboot-box {
        width: 76; max-width: 90%; height: auto; padding: 1 2;
        border: thick $error; background: $surface;
    }
    """
    BINDINGS = [
        Binding("1", "order('1')", "serial"),
        Binding("2", "order('2')", "rolling"),
        Binding("3", "order('3')", "all-at-once"),
        Binding("d", "toggle_drain", "toggle drain"),
        Binding("y", "run", "run"),
        Binding("escape,n", "cancel", "cancel"),
    ]

    def __init__(self, target: str) -> None:
        super().__init__()
        self._target = target
        self._order = "1"
        self._drain = True

    def compose(self) -> ComposeResult:
        yield Vertical(Static(id="reboot-text"), id="reboot-box")

    def on_mount(self) -> None:
        self._refresh()

    def action_order(self, key: str) -> None:
        self._order = key
        self._refresh()

    def action_toggle_drain(self) -> None:
        self._drain = not self._drain
        self._refresh()

    def action_run(self) -> None:
        serial = next(s for k, _, s in self._ORDERS if k == self._order)
        self.dismiss({"serial": serial, "drain": self._drain})

    def action_cancel(self) -> None:
        self.dismiss(None)

    def _refresh(self) -> None:
        text = Text()
        text.append(f"Reboot '{self._target}'?\n\n", style="bold")
        text.append("Order ", style="bold")
        text.append("(1/2/3)\n")
        for key, label, _ in self._ORDERS:
            on = key == self._order
            text.append("  ")
            text.append("●" if on else "○", style="bold green" if on else "dim")
            text.append(f" {label}\n", style="bold" if on else "dim")
        text.append("\nk8s ", style="bold")
        text.append("(d)\n  ")
        text.append("●" if self._drain else "○", style="bold green" if self._drain else "dim")
        text.append(
            " Drain-safe: cordon & drain k8s nodes first"
            if self._drain
            else " Skip drain: reboot k8s nodes without draining",
            style="bold" if self._drain else "dim",
        )
        text.append("\n\n")
        text.append("y", style="bold red")
        text.append(" to run   ")
        text.append("esc", style="bold")
        text.append(" to cancel")
        self.query_one("#reboot-text", Static).update(text)


class TaskScreen(Screen):
    """Run argv, stream output, report the exit code. Esc terminates a
    still-running task before leaving."""

    BINDINGS = [Binding("escape,q", "close", "back (terminates if running)")]

    def __init__(
        self,
        title: str,
        argv: list[str],
        cwd: Path,
        env: dict[str, str] | None = None,
    ) -> None:
        super().__init__()
        self._title = title
        self._argv = argv
        self._cwd = cwd
        self._env = env or {}
        self._proc: subprocess.Popen | None = None
        self._lines: deque[str] = deque(maxlen=8000)
        self._rendered = 0

    def compose(self) -> ComposeResult:
        yield Header(show_clock=True)
        yield RichLog(wrap=True, max_lines=8000)
        yield Footer()

    def on_mount(self) -> None:
        self.app.sub_title = self._title
        self._lines.append(f"$ {' '.join(self._argv)}  (cwd={self._cwd})")
        env = dict(os.environ, PYTHONUNBUFFERED="1", ANSIBLE_FORCE_COLOR="0", **self._env)
        try:
            self._proc = subprocess.Popen(
                self._argv,
                cwd=self._cwd,
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
            )
        except OSError as e:
            self._lines.append(f"failed to launch: {e}")
            return

        def drain() -> None:
            assert self._proc is not None and self._proc.stdout is not None
            for line in self._proc.stdout:
                self._lines.append(line.rstrip("\n"))
            rc = self._proc.wait()
            self._lines.append(f"— exit {rc}")

        threading.Thread(target=drain, daemon=True).start()
        # NB: must not be called `_render` — that overrides textual's
        # internal Widget._render (returns a Visual) and blanks the app.
        self.set_interval(0.2, self._pump)

    def _pump(self) -> None:
        log = self.query_one(RichLog)
        lines = list(self._lines)
        for line in lines[self._rendered:]:
            log.write(to_text(line))
        self._rendered = len(lines)

    def action_close(self) -> None:
        if self._proc is not None and self._proc.poll() is None:
            self._proc.terminate()
        # dismiss (not pop): callers that pushed with a callback get the
        # exit code and can refresh their views.
        self.dismiss(None if self._proc is None else self._proc.poll())


class AttachedLogScreen(Screen):
    """Read-only observer of a registered command run (an MCP-launched
    reboot): tail its output.log live and report liveness/exit from the
    run registry. Never owns the subprocess — esc just detaches, the
    run keeps going."""

    BINDINGS = [Binding("escape,q", "close", "detach (run keeps going)")]

    def __init__(self, title: str, run_id: str) -> None:
        super().__init__()
        self._title = title
        self._run_id = run_id
        self._offset = 0
        self._settled = False

    def compose(self) -> ComposeResult:
        yield Header(show_clock=True)
        yield RichLog(wrap=True, max_lines=8000)
        yield Footer()

    def on_mount(self) -> None:
        self.app.sub_title = self._title
        self.set_interval(0.5, self._pump)
        self._pump()

    def _pump(self) -> None:
        from ..registry import RunLiveness, open_run
        from ..runner import COMMAND_LOG

        log = self.query_one(RichLog)
        obs = open_run(self._run_id)
        if obs is None:
            if not self._settled:
                self._settled = True
                log.write(Text(f"run {self._run_id} is gone (pruned?)", style="bold red"))
            return
        path = obs.info.path / COMMAND_LOG
        try:
            with open(path, "r", encoding="utf-8", errors="replace") as fh:
                fh.seek(self._offset)
                chunk = fh.read()
                self._offset = fh.tell()
        except OSError:
            chunk = ""
        for line in chunk.splitlines():
            log.write(to_text(line))
        if self._settled:
            return
        liveness = obs.liveness()
        if liveness is not RunLiveness.RUNNING:
            self._settled = True
            rc = obs.info.meta.get("rc")
            style = "bold green" if liveness is RunLiveness.FINISHED else "bold red"
            log.write(Text(f"— {liveness.value} (rc={rc})", style=style))

    def action_close(self) -> None:
        from ..registry import RunLiveness, open_run

        # An observer never terminates the run; hand back its rc (None
        # while still running) so _after_mutation can refresh drift once
        # a finished reboot's screen closes.
        obs = open_run(self._run_id)
        rc = None
        if obs is not None and obs.liveness() is not RunLiveness.RUNNING:
            rc = obs.info.meta.get("rc")
        self.dismiss(rc)
