"""FastMCP server smoke + read-tool parity over an injected aggregate.

No nix eval: the inventory's cached aggregate is injected (as in
test_inventory), so these run headless — the server is pure delegation to
the cores. Async tools are exercised through FastMCP's in-memory client via
asyncio.run(), so no pytest-asyncio plugin is needed.
"""

import asyncio

from fastmcp import Client

from mandala_fleet.inventory import Inventory
from mandala_fleet.mcp import build_server


def _inv() -> Inventory:
    inv = Inventory(flake=".")
    inv.__dict__["aggregate"] = {
        "schemaVersion": 1,
        "members": {"web": {"platform": "metal"}, "cache": {}, "router": {}},
        "groups": {"k3s": ["cache", "web"], "gateway": ["router"]},
        "projections": {"deploy": {"nodes": ["cache", "web"]}},
    }
    return inv


def _call(mcp, name: str, args: dict):
    async def go():
        async with Client(mcp) as client:
            return await client.call_tool(name, args)

    return asyncio.run(go()).data


def test_lists_read_drift_and_action_tools() -> None:
    mcp = build_server(_inv())

    async def _list() -> list:
        async with Client(mcp) as client:
            return await client.list_tools()

    names = {t.name for t in asyncio.run(_list())}
    assert {
        "members", "groups", "resolve", "ping", "host_eval", "drift",
        "deploy_status", "build", "deploy", "reboot",
    } <= names


def test_deploy_real_activation_refused_without_confirm() -> None:
    inv = _inv()
    data = _call(build_server(inv), "deploy", {"selector": "@k3s", "dry_activate": False})
    assert data["refused"] is True
    # The model must restate exactly the resolved target to activate.
    assert data["required_confirm"] == inv.to_limit("@k3s") == "cache,web"


def test_reboot_refused_without_matching_confirm() -> None:
    data = _call(build_server(_inv()), "reboot", {"selector": "@k3s", "confirm": "web"})
    assert data["refused"] is True
    assert data["required_confirm"] == "cache,web"


def test_deploy_dry_run_launches_without_confirm(monkeypatch, tmp_path) -> None:
    monkeypatch.setenv("MANDALA_FLEET_STATE", str(tmp_path))
    from mandala_fleet import runner

    # Stub the launch: allocate the registry run dir + run_id, no subprocess.
    monkeypatch.setattr(runner.DeployRun, "start", lambda self: self.resolve_paths())

    data = _call(build_server(_inv()), "deploy", {"selector": "@k3s"})
    assert data["ok"] is True and data["dry_activate"] is True
    assert data["run_id"]


def test_deploy_status_sees_out_of_band_run(monkeypatch, tmp_path) -> None:
    import json

    monkeypatch.setenv("MANDALA_FLEET_STATE", str(tmp_path))
    from mandala_fleet import registry

    run_id, path = registry.new_run_dir()
    registry.write_meta(path, {"run_id": run_id, "pid": 1, "limit": "cache,web"})
    with open(path / "cache.jsonl", "a", encoding="utf-8") as fh:
        for m in ("eval", "build", "copy", "activate", "confirm"):
            fh.write(json.dumps(
                {"v": 1, "host": "cache", "plugin": "deploy",
                 "event": "milestone", "milestone": m}) + "\n")

    data = _call(build_server(_inv()), "deploy_status", {"run_id": run_id})
    assert data["hosts"]["cache"]["state"] == "confirmed"


def test_activity_sink_records_each_call() -> None:
    seen: list[dict] = []
    mcp = build_server(_inv(), activity_sink=seen.append)
    _call(mcp, "resolve", {"selector": "@k3s"})
    assert any(e["tool"] == "resolve" and e["status"] == "ok" for e in seen)


def test_token_asgi_middleware_guards_http() -> None:
    from mandala_fleet.mcp.host import _TokenASGIMiddleware

    reached: list[str] = []

    async def inner(scope, receive, send):
        reached.append(scope["type"])

    mw = _TokenASGIMiddleware(inner, token="s3cret")

    async def receive():
        return {}

    # No token → 401, inner app never reached.
    sent: list[dict] = []
    asyncio.run(mw({"type": "http", "headers": []}, receive, lambda m: _append(sent, m)))
    assert reached == [] and sent[0]["status"] == 401

    # Matching token → passes through to the inner app.
    ok_scope = {"type": "http", "headers": [(b"authorization", b"Bearer s3cret")]}
    asyncio.run(mw(ok_scope, receive, lambda m: _append(sent, m)))
    assert reached == ["http"]

    # Non-HTTP scopes (lifespan) pass through untouched.
    asyncio.run(mw({"type": "lifespan"}, receive, lambda m: _append(sent, m)))
    assert reached == ["http", "lifespan"]


async def _append(bucket: list, message: dict) -> None:
    bucket.append(message)


def test_resolve_tool_parity_with_core() -> None:
    inv = _inv()
    mcp = build_server(inv)

    async def _call() -> object:
        async with Client(mcp) as client:
            return await client.call_tool("resolve", {"selector": "@k3s"})

    result = asyncio.run(_call())
    assert result.data == inv.resolve("@k3s") == ["cache", "web"]
