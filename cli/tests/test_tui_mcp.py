"""TUI ↔ MCP activity integration, driven headless via Textual's pilot.

A hosted tool call must render like a keypress: its start event puts a
braille job in the status bar (as pressing S does for eval/survey) and a
line in the panel's pending strip; the settle event pops both and appends
the history line. No MCP host is bound — events are posted directly.
"""

import asyncio

from mandala_fleet.inventory import Inventory
from mandala_fleet.tui.explorer import ExplorerApp, McpActivity, McpInventorySwap


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


def test_quit_signals_mcp_shutdown(monkeypatch, tmp_path) -> None:
    """`q` must stop the embedded host ORDERLY before the app exits —
    an abrupt worker cancel leaves uvicorn's socket accepting while the
    streamable-http task group is already gone (the post-quit "Task group
    is not initialized" spew)."""
    monkeypatch.setenv("MANDALA_FLEET_STATE", str(tmp_path))
    from mandala_fleet import drift as drift_mod

    monkeypatch.setattr(drift_mod, "repo_rev", lambda flake: "deadbeef")

    stopped: list[str] = []

    async def fake_host(self) -> None:
        # Stand-in for serve_http's graceful path: runs until the
        # shutdown event fires, records that it exited cleanly.
        await self._mcp_shutdown.wait()
        stopped.append("clean")

    monkeypatch.setattr(ExplorerApp, "_serve_mcp_host", fake_host)

    async def go() -> None:
        app = ExplorerApp(_inv(), serve_mcp=True)
        async with app.run_test() as pilot:
            await pilot.pause()
            assert not app._mcp_shutdown.is_set()
            await pilot.press("q")
        assert app._mcp_shutdown.is_set()
        assert stopped == ["clean"]

    asyncio.run(go())


def test_mcp_inventory_swap_repaints(monkeypatch, tmp_path) -> None:
    """The `reload` tool's swap message adopts the fresh inventory and
    repaints the tables — like pressing `r`, minus the re-eval."""
    monkeypatch.setenv("MANDALA_FLEET_STATE", str(tmp_path))
    from mandala_fleet import drift as drift_mod

    monkeypatch.setattr(drift_mod, "repo_rev", lambda flake: "deadbeef")

    async def noop_host(self) -> None:
        return None

    monkeypatch.setattr(ExplorerApp, "_serve_mcp_host", noop_host)

    fresh = Inventory(flake=".")
    fresh.__dict__["aggregate"] = {
        "schemaVersion": 1,
        "members": {"web": {}, "newbie": {}},
        "groups": {"k3s": ["web"]},
        "projections": {"deploy": {"nodes": ["web"]}},
    }

    async def go() -> None:
        app = ExplorerApp(_inv(), serve_mcp=True)
        async with app.run_test() as pilot:
            await pilot.pause()
            app.post_message(McpInventorySwap(fresh))
            await pilot.pause()
            # The load worker runs off-thread; give it a beat.
            deadline = asyncio.get_event_loop().time() + 5
            table = app.query_one("#members-table")
            while table.row_count != 2 and asyncio.get_event_loop().time() < deadline:
                await pilot.pause(0.05)
            assert app.inventory is fresh
            assert table.row_count == 2

    asyncio.run(go())
