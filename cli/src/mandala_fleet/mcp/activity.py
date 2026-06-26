"""Activity middleware: surface every tool call to a sink.

When the server is hosted by the TUI, the sink posts each call into the
Claude-activity pane so the operator watches what the client is doing —
the tool name, its arguments, and whether it succeeded or errored. The
sink must never break a tool call, so its exceptions are swallowed.
"""

from __future__ import annotations

from typing import Any, Callable

from fastmcp.server.middleware import Middleware

ActivitySink = Callable[[dict[str, Any]], None]


class ActivityMiddleware(Middleware):
    """Emit `{tool, args, status, detail}` per tool call to a sink."""

    def __init__(self, sink: ActivitySink) -> None:
        self._sink = sink

    async def on_call_tool(self, context, call_next):  # type: ignore[no-untyped-def]
        params = context.message
        name = getattr(params, "name", "?")
        args = dict(getattr(params, "arguments", {}) or {})
        try:
            result = await call_next(context)
        except Exception as e:  # noqa: BLE001 — re-raised after emitting
            self._emit(name, args, "error", str(e))
            raise
        self._emit(name, args, "ok", None)
        return result

    def _emit(self, name: str, args: dict, status: str, detail: str | None) -> None:
        try:
            self._sink({"tool": name, "args": args, "status": status, "detail": detail})
        except Exception:  # noqa: BLE001 — the feed must never sink a call
            pass
