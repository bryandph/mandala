"""TUI ↔ MCP activity integration, driven headless via Textual's pilot.

A hosted tool call must render like a keypress: its start event puts a
braille job in the status bar (as pressing S does for eval/survey) and a
line in the panel's pending strip; the settle event pops both and appends
the history line. No MCP host is bound — events are posted directly.
"""

import asyncio

from mandala_fleet.inventory import Inventory
from mandala_fleet.tui.explorer import ExplorerApp, McpActivity


def _inv() -> Inventory:
    inv = Inventory(flake=".")
    inv.__dict__["aggregate"] = {
        "schemaVersion": 1,
        "members": {"web": {"platform": "metal"}, "cache": {}},
        "groups": {"k3s": ["cache", "web"]},
        "projections": {"deploy": {"nodes": ["cache", "web"]}},
    }
    return inv


def test_pending_call_spins_then_settles(monkeypatch, tmp_path) -> None:
    monkeypatch.setenv("MANDALA_FLEET_STATE", str(tmp_path))
    from mandala_fleet import drift as drift_mod

    # No git in the build sandbox; the rev only keys the expected cache.
    monkeypatch.setattr(drift_mod, "repo_rev", lambda flake: "deadbeef")

    async def noop_host(self) -> None:
        return None

    monkeypatch.setattr(ExplorerApp, "_serve_mcp_host", noop_host)

    async def go() -> None:
        app = ExplorerApp(_inv(), serve_mcp=True)
        async with app.run_test() as pilot:
            await pilot.pause()
            strip = app.query_one("#mcp-pending")
            assert not strip.display  # nothing pending → strip collapsed

            app.post_message(McpActivity(
                {"tool": "drift", "args": {"refresh": True},
                 "status": "start", "detail": None, "seq": 7}
            ))
            await pilot.pause()
            assert "mcp drift" in app.sub_title  # spins like pressing S
            assert strip.display

            app.post_message(McpActivity(
                {"tool": "drift", "args": {"refresh": True},
                 "status": "ok", "detail": None, "seq": 7}
            ))
            await pilot.pause()
            assert not strip.display
            assert "mcp drift" not in app.sub_title
            # The settled drift(refresh) call refreshed the drift view the
            # way an operator S-refresh does.
            assert app.sub_title == "drift refreshed (mcp)"

    asyncio.run(go())
