"""Shared render helpers for TUI panes."""

from __future__ import annotations

import re

from rich.text import Text

# deploy-rs / nix / ansible output carries cursor-control CSI sequences
# (erase-line ESC[K, cursor moves) besides SGR colors. Keep the colors
# (Text.from_ansi understands SGR), drop everything else — rendered raw
# they shred the panes.
_CSI_RE = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]")
# C0 controls except ESC (SGR survives for from_ansi) and tab.
_CTRL_RE = re.compile(r"[\x00-\x08\x0b-\x1a\x1c-\x1f\x7f]")


def to_text(line: str) -> Text:
    cleaned = _CSI_RE.sub(lambda m: m.group(0) if m.group(0).endswith("m") else "", line)
    cleaned = _CTRL_RE.sub("", cleaned)
    return Text.from_ansi(cleaned)
