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
