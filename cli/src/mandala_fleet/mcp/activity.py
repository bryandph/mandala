"""Activity middleware: surface every tool call to a sink.

When the server is hosted by the TUI, the sink posts each call into the
Claude-activity pane so the operator watches what the client is doing —
the tool name, its arguments, and whether it succeeded or errored. Every
call emits TWICE: a `start` event as the tool begins (so a long eval or
survey is visible while it runs, not only after it lands) and an
`ok`/`error` event when it settles; the two share a `seq` so a watcher
can pair them even with concurrent same-named calls in flight. The settle
event also carries `elapsed` (seconds) and a small `result` summary
(ok/refused/run_id when the tool returned them) — enough for the pane to
show durations and for the TUI to attach the exact run a tool registered.
The sink must never break a tool call, so its exceptions are swallowed.

`audit_event` is the persistent trail: settled MUTATING calls are appended
to `state_dir()/mcp/audit.jsonl` regardless of transport, so a headless
stdio session leaves the same record the TUI operator watches live.
"""

from __future__ import annotations

import itertools
import json
import time
from typing import Any, Callable

from fastmcp.server.middleware import Middleware

ActivitySink = Callable[[dict[str, Any]], None]

# Tools whose settled calls land in the audit trail: everything that can
# change fleet state (or swap what the server serves).
_AUDITED = frozenset({"deploy", "reboot", "restart_service", "reload"})


def _result_summary(result: Any) -> dict | None:
    """The few result fields a watcher acts on (ok/refused/run_id), pulled
    defensively from FastMCP's ToolResult structured content."""
    data = getattr(result, "structured_content", None)
    if isinstance(data, dict) and set(data) == {"result"} and isinstance(data["result"], dict):
        data = data["result"]  # unwrapped non-object returns
    if not isinstance(data, dict):
        return None
    summary = {k: data[k] for k in ("ok", "refused", "run_id") if k in data}
    return summary or None


def audit_event(event: dict) -> None:
    """Append a settled mutating call to the per-user audit log. Best
    effort — an unwritable state dir must never sink a tool call."""
    if event.get("status") == "start" or event.get("tool") not in _AUDITED:
        return
    from ..drift import state_dir

    path = state_dir() / "mcp" / "audit.jsonl"
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        with open(path, "a", encoding="utf-8") as fh:
            fh.write(json.dumps({"ts": time.time(), **event}) + "\n")
    except (OSError, TypeError, ValueError):
        pass


class ActivityMiddleware(Middleware):
    """Emit `{tool, args, status, detail, seq}` per tool call to a sink —
    `status: "start"` at call entry, then `"ok"`/`"error"` (plus `elapsed`
    and a `result` summary) at settle."""

    def __init__(self, sink: ActivitySink) -> None:
        self._sink = sink
        self._seq = itertools.count(1)

    async def on_call_tool(self, context, call_next):  # type: ignore[no-untyped-def]
        params = context.message
        name = getattr(params, "name", "?")
        args = dict(getattr(params, "arguments", {}) or {})
        seq = next(self._seq)
        started = time.monotonic()
        self._emit({
            "tool": name, "args": args, "status": "start",
            "detail": None, "seq": seq,
        })
        try:
            result = await call_next(context)
        except Exception as e:  # noqa: BLE001 — re-raised after emitting
            self._emit({
                "tool": name, "args": args, "status": "error",
                "detail": str(e), "seq": seq,
                "elapsed": round(time.monotonic() - started, 3),
            })
            raise
        self._emit({
            "tool": name, "args": args, "status": "ok",
            "detail": None, "seq": seq,
            "elapsed": round(time.monotonic() - started, 3),
            "result": _result_summary(result),
        })
        return result

    def _emit(self, event: dict) -> None:
        try:
            self._sink(event)
        except Exception:  # noqa: BLE001 — the feed must never sink a call
            pass
