"""nom hosted on a pane-sized PTY, rendered through a pyte screen.

The build plugin streams nix's verbatim internal-json over the event
channel (protocol v2 `nixlog`); this widget runs a LOCAL `nom --json`
whose stdout is a PTY we own, feeds it those lines, and renders the
emulated screen — the real nom tree, colors and all, inside a tab.
Falls back to a notice (the summary pane still works) when nom isn't on
PATH.
"""

from __future__ import annotations

import fcntl
import os
import signal
import struct
import subprocess
import termios
import threading

from rich.style import Style
from rich.text import Text
from textual.widgets import Static

_NAMED = {
    "black": "black", "red": "red", "green": "green", "brown": "yellow",
    "blue": "blue", "magenta": "magenta", "cyan": "cyan", "white": "white",
    "brightblack": "bright_black", "brightred": "bright_red",
    "brightgreen": "bright_green", "brightbrown": "bright_yellow",
    "brightyellow": "bright_yellow", "brightblue": "bright_blue",
    "brightmagenta": "bright_magenta", "brightcyan": "bright_cyan",
    "brightwhite": "bright_white",
}


def _color(value: str) -> str | None:
    if not value or value == "default":
        return None
    if value in _NAMED:
        return _NAMED[value]
    if len(value) == 6 and all(c in "0123456789abcdefABCDEF" for c in value):
        return f"#{value}"
    return None


class NomPane(Static):
    """Feed internal-json lines via .feed(); everything else is automatic."""

    # Static defaults to height:auto, which would size this pane to its own
    # rendered content — the pyte screen — locking the PTY at the _dims()
    # floor forever. 1fr claims the tab's full height so content_size (and
    # therefore nom's terminal) tracks the actual available space.
    DEFAULT_CSS = "NomPane { height: 1fr; }"

    def __init__(self, **kwargs) -> None:
        super().__init__("", **kwargs)
        self._proc: subprocess.Popen | None = None
        self._master_fd: int | None = None
        self._screen = None
        self._stream = None
        self._lock = threading.Lock()
        self._failed: str | None = None
        self._pending: list[str] | None = []  # lines fed before nom spawned

    # -- lifecycle ---------------------------------------------------------

    def on_mount(self) -> None:
        self.call_after_refresh(self._spawn)
        self.set_interval(0.2, self._render_screen)

    def _dims(self) -> tuple[int, int]:
        size = self.content_size
        return (max(size.height, 10), max(size.width, 40))

    def _spawn(self) -> None:
        import pyte

        rows, cols = self._dims()
        self._screen = pyte.Screen(cols, rows)
        self._stream = pyte.ByteStream(self._screen)
        master, slave = os.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        env = dict(os.environ, TERM="xterm-256color")
        try:
            self._proc = subprocess.Popen(
                ["nom", "--json"],
                stdin=subprocess.PIPE,
                stdout=slave,
                stderr=slave,
                env=env,
                bufsize=0,
            )
        except OSError as e:
            self._failed = f"nom unavailable ({e}) — the summary pane above still tracks the build"
            self._pending = None
            os.close(master)
            os.close(slave)
            return
        finally:
            if self._proc is not None:
                os.close(slave)
        self._master_fd = master
        threading.Thread(target=self._drain_pty, daemon=True).start()
        pending, self._pending = self._pending, None
        for line in pending or []:
            self.feed(line)

    def _drain_pty(self) -> None:
        assert self._master_fd is not None
        while True:
            try:
                chunk = os.read(self._master_fd, 8192)
            except OSError:
                break
            if not chunk:
                break
            with self._lock:
                try:
                    self._stream.feed(chunk)
                except Exception:  # noqa: BLE001 — emulator hiccups must not kill the reader
                    pass

    # -- input -------------------------------------------------------------

    def feed(self, line: str) -> None:
        """One raw '@nix {...}' line into nom's stdin."""
        if self._pending is not None:
            self._pending.append(line)
            return
        if self._proc is None or self._proc.stdin is None:
            return
        try:
            self._proc.stdin.write((line + "\n").encode("utf-8"))
            self._proc.stdin.flush()
        except (BrokenPipeError, ValueError, OSError):
            pass

    def finish(self) -> None:
        """EOF nom's stdin so it draws its final summary and exits."""
        if self._proc is not None and self._proc.stdin is not None:
            try:
                self._proc.stdin.close()
            except OSError:
                pass

    # -- output ------------------------------------------------------------

    def _render_screen(self) -> None:
        if self._failed is not None:
            self.update(Text(self._failed, style="dim"))
            return
        if self._screen is None:
            return
        with self._lock:
            if not self._screen.dirty:
                return
            self._screen.dirty.clear()
            buffer = self._screen.buffer
            rows, cols = self._screen.lines, self._screen.columns
            text = Text()
            for y in range(rows):
                row = buffer[y]
                # Run-length coalesce same-styled cells: a full screen of
                # per-char Styles five times a second is needless churn.
                run, run_key = [], None
                for x in range(cols):
                    ch = row[x]
                    key = (ch.fg, ch.bg, ch.bold, ch.italics, ch.reverse)
                    if key != run_key and run:
                        text.append("".join(run), style=self._style(run_key))
                        run = []
                    run_key = key
                    run.append(ch.data or " ")
                if run:
                    text.append("".join(run), style=self._style(run_key))
                if y < rows - 1:
                    text.append("\n")
        self.update(text)

    @staticmethod
    def _style(key) -> Style:
        fg, bg, bold, italics, reverse = key
        return Style(
            color=_color(fg), bgcolor=_color(bg),
            bold=bold, italic=italics, reverse=reverse,
        )

    def on_resize(self) -> None:
        if self._screen is None or self._master_fd is None:
            return
        rows, cols = self._dims()
        with self._lock:
            self._screen.resize(rows, cols)
        try:
            fcntl.ioctl(self._master_fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
            if self._proc is not None and self._proc.poll() is None:
                self._proc.send_signal(signal.SIGWINCH)
        except OSError:
            pass

    def on_unmount(self) -> None:
        self.finish()
        if self._proc is not None and self._proc.poll() is None:
            self._proc.terminate()
        if self._master_fd is not None:
            try:
                os.close(self._master_fd)
            except OSError:
                pass
