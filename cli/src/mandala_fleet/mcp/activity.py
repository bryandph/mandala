"""Activity middleware: surface every tool call to a sink.

When the server is hosted by the TUI, the sink posts each call into the
Claude-activity pane so the operator watches what the client is doing —
the tool name, its arguments, and whether it succeeded or errored. Every
call emits TWICE: a `start` event as the tool begins (so a long eval or
survey is visible while it runs, not only after it lands) and an
`ok`/`error` event when it settles; the two share a `seq` so a watcher
can pair them even with concurrent same-named calls in flight. The sink
must never break a tool call, so its exceptions are swallowed.
"""

from __future__ import annotations

import itertools
from typing import Any, Callable

from fastmcp.server.middleware import Middleware

ActivitySink = Callable[[dict[str, Any]], None]


class ActivityMiddleware(Middleware):
    """Emit `{tool, args, status, detail, seq}` per tool call to a sink —
    `status: "start"` at call entry, then `"ok"`/`"error"` at settle."""

    def __init__(self, sink: ActivitySink) -> None:
        self._sink = sink
        self._seq = itertools.count(1)

    async def on_call_tool(self, context, call_next):  # type: ignore[no-untyped-def]
        params = context.message
        name = getattr(params, "name", "?")
        args = dict(getattr(params, "arguments", {}) or {})
        seq = next(self._seq)
        self._emit(name, args, "start", None, seq)
        try:
            result = await call_next(context)
        except Exception as e:  # noqa: BLE001 — re-raised after emitting
            self._emit(name, args, "error", str(e), seq)
            raise
        self._emit(name, args, "ok", None, seq)
        return result

    def _emit(
        self, name: str, args: dict, status: str, detail: str | None, seq: int
    ) -> None:
        try:
            self._sink(
                {"tool": name, "args": args, "status": status, "detail": detail, "seq": seq}
            )
        except Exception:  # noqa: BLE001 — the feed must never sink a call
            pass
