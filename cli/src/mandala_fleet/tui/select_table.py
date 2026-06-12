"""DataTable with file-manager multi-select.

    space          toggle the cursor row in/out of the selection
    shift+up/down  contiguous range from the anchor (last toggle/cursor)
    ctrl+up/down   move the cursor WITHOUT touching the selection
    escape         clear the selection

Selection is the union of individually toggled rows (sticky) and the
current shift-range. Rows are registered by NAME (add_named_row), so the
selection survives cursor churn; a marker column renders membership.
"""

from __future__ import annotations

from rich.text import Text
from textual.binding import Binding
from textual.coordinate import Coordinate
from textual.widgets import DataTable

_MARK = Text("●", style="bold cyan")
_NO_MARK = Text(" ")


class SelectTable(DataTable):
    BINDINGS = [
        Binding("space", "toggle_select", "select"),
        Binding("shift+up", "extend_select(-1)", "extend ↑", show=False),
        Binding("shift+down", "extend_select(1)", "extend ↓", show=False),
        Binding("ctrl+up", "skip(-1)", show=False),
        Binding("ctrl+down", "skip(1)", show=False),
        Binding("escape", "clear_select", "clear selection", show=False),
    ]

    def __init__(self, **kwargs) -> None:
        super().__init__(**kwargs)
        self._names: list[str] = []
        self.selected: set[str] = set()
        self._sticky: set[str] = set()
        self._anchor: int | None = None

    # -- rows ------------------------------------------------------------

    def reset_rows(self) -> None:
        """Clear rows AND selection state (call before a refill)."""
        self.clear()
        self._names = []
        self.selected = set()
        self._sticky = set()
        self._anchor = None

    def add_named_row(self, name: str, *cells) -> None:
        """Register a row under `name`; the marker column is prepended."""
        self._names.append(name)
        self.add_row(_NO_MARK, *cells)

    @property
    def cursor_name(self) -> str | None:
        if not self._names:
            return None
        return self._names[min(self.cursor_row, len(self._names) - 1)]

    @property
    def selected_names(self) -> list[str]:
        """Selected names in table order."""
        return [n for n in self._names if n in self.selected]

    # -- selection mechanics ----------------------------------------------

    def _apply(self, new: set[str]) -> None:
        old, self.selected = self.selected, new
        for idx, name in enumerate(self._names):
            if (name in old) != (name in new):
                self.update_cell_at(
                    Coordinate(idx, 0), _MARK if name in new else _NO_MARK
                )

    def action_toggle_select(self) -> None:
        name = self.cursor_name
        if name is None:
            return
        self._sticky = self._sticky ^ {name}
        self._anchor = self.cursor_row
        self._apply(set(self._sticky))

    def action_extend_select(self, delta: int) -> None:
        if not self._names:
            return
        if self._anchor is None:
            self._anchor = self.cursor_row
        row = max(0, min(len(self._names) - 1, self.cursor_row + delta))
        self.move_cursor(row=row)
        lo, hi = sorted((self._anchor, row))
        self._apply(self._sticky | {self._names[i] for i in range(lo, hi + 1)})

    def action_skip(self, delta: int) -> None:
        if not self._names:
            return
        row = max(0, min(len(self._names) - 1, self.cursor_row + delta))
        self.move_cursor(row=row)

    def action_clear_select(self) -> None:
        self._sticky = set()
        self._anchor = None
        self._apply(set())
