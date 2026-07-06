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


def test_members_compact_by_default_full_on_request() -> None:
    mcp = build_server(_inv())
    compact = _call(mcp, "members", {})
    # Compact: only the whitelisted keys + surfaces, never the full dump.
    assert compact["web"] == {"platform": "metal", "surfaces": "---"}
    full = _call(mcp, "members", {"full": True})
    assert full["web"] == {"platform": "metal"}


def test_selector_taxonomy_reaches_the_tools() -> None:
    # `all` and `!` exclusions resolve through the same core the gated
    # actions confirm against.
    data = _call(build_server(_inv()), "resolve", {"selector": "all,!@gateway"})
    assert data["members"] == ["cache", "web"]
    assert data["limit"] == "cache,web"


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


def test_reboot_launches_registered_background_run(monkeypatch, tmp_path) -> None:
    """A confirmed reboot returns run_id immediately (no blocking on the
    playbook) and registers a kind=reboot run — observable via
    deploy_status and the TUI even if this client times out."""
    monkeypatch.setenv("MANDALA_FLEET_STATE", str(tmp_path))
    import shutil

    monkeypatch.setattr(
        shutil, "which", lambda name: "/fake/ans-reboot" if name == "ans-reboot" else None
    )

    class _FakePopen:
        pid = 54321

        def __init__(self, argv, **kwargs):
            self.argv = argv

        def wait(self):
            return 0

    from mandala_fleet import registry, runner

    monkeypatch.setattr(runner.subprocess, "Popen", _FakePopen)
    data = _call(
        build_server(_inv()),
        "reboot",
        {"selector": "@k3s", "serial": "2", "confirm": "cache,web"},
    )
    assert data["ok"] is True and data["run_id"]
    assert data["log"].endswith("output.log")

    # The reaper thread records rc=0 (the fake wait) — poll until it lands.
    import time

    path = registry.runs_dir() / data["run_id"]
    deadline = time.monotonic() + 10
    while "rc" not in (meta := registry.read_meta(path)):
        assert time.monotonic() < deadline, "reaper never recorded rc"
        time.sleep(0.05)
    assert meta["kind"] == "reboot" and meta["limit"] == "cache,web"
    assert meta["argv"][0] == "ans-reboot"
    assert "reboot_serial=2" in meta["argv"]


def test_deploy_status_reports_command_runs(monkeypatch, tmp_path) -> None:
    monkeypatch.setenv("MANDALA_FLEET_STATE", str(tmp_path))
    from mandala_fleet import registry
    from mandala_fleet.runner import COMMAND_LOG

    run_id, path = registry.new_run_dir()
    registry.write_meta(
        path, {"run_id": run_id, "kind": "reboot", "pid": None, "rc": 2, "limit": "web"}
    )
    (path / COMMAND_LOG).write_text("$ ans-reboot -l web\nfatal: boom\n")

    data = _call(build_server(_inv()), "deploy_status", {"run_id": run_id})
    assert data["kind"] == "reboot"
    assert data["liveness"] == "failed" and data["phase"] == "done"
    assert data["output_tail"][-1] == "fatal: boom"
    # Nothing host/build-shaped leaks into a command-run snapshot.
    assert "hosts" not in data and "build" not in data


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


def test_activity_sink_records_start_then_settle() -> None:
    """Every call emits start → ok/error sharing a seq, so the TUI can show
    the call as PENDING (spinner) while it runs and pop it when it lands."""
    seen: list[dict] = []
    mcp = build_server(_inv(), activity_sink=seen.append)
    _call(mcp, "resolve", {"selector": "@k3s"})
    events = [e for e in seen if e["tool"] == "resolve"]
    assert [e["status"] for e in events] == ["start", "ok"]
    assert events[0]["seq"] == events[1]["seq"]
    assert events[0]["args"] == {"selector": "@k3s"}


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
    # `limit` is the exact confirm string the gated actions require.
    assert result.data["members"] == inv.resolve("@k3s") == ["cache", "web"]
    assert result.data["limit"] == inv.to_limit("@k3s") == "cache,web"


class _FakeProc:
    def __init__(self, stdout: str = "", stderr: str = "", returncode: int = 0):
        self.stdout, self.stderr, self.returncode = stdout, stderr, returncode


def test_ping_separates_stderr_diagnostics(monkeypatch) -> None:
    """stderr (deprecation chatter, side-band git progress ansible relabels
    as [ERROR]) must not pollute `output` — it rides in `diagnostics`."""
    from mandala_fleet.mcp import server as server_mod

    fake = _FakeProc(
        stdout='web | SUCCESS => {"ping": "pong"}\n',
        stderr="[ERROR]: remote: Counting objects: 100% (14/14)\n",
    )
    monkeypatch.setattr(
        server_mod.subprocess, "run", lambda *a, **k: fake
    )
    data = _call(build_server(_inv()), "ping", {"selector": "web"})
    assert data["reachable"] == {"web": True}
    assert "Counting objects" not in data["output"]
    assert "Counting objects" in data["diagnostics"]


def test_restart_service_refused_without_confirm() -> None:
    data = _call(
        build_server(_inv()),
        "restart_service",
        {"selector": "@k3s", "unit": "k3s"},
    )
    assert data["refused"] is True
    assert data["required_confirm"] == "cache,web"


def test_restart_service_rejects_unclean_unit_names() -> None:
    import pytest

    with pytest.raises(Exception, match="unit name"):
        _call(
            build_server(_inv()),
            "restart_service",
            {"selector": "@k3s", "unit": "k3s state=stopped", "confirm": "cache,web"},
        )


def test_restart_service_runs_and_parses_hosts(monkeypatch) -> None:
    from mandala_fleet.mcp import server as server_mod

    seen: dict = {}

    def fake_run(argv, **kwargs):
        seen["argv"] = argv
        return _FakeProc(
            stdout="cache | CHANGED => {}\nweb | FAILED! => {}\n", returncode=2
        )

    monkeypatch.setattr(server_mod.subprocess, "run", fake_run)
    data = _call(
        build_server(_inv()),
        "restart_service",
        {"selector": "@k3s", "unit": "k3s", "confirm": "cache,web"},
    )
    assert seen["argv"][:2] == ["ansible", "cache,web"]
    assert "ansible.builtin.systemd_service" in seen["argv"]
    assert data["restarted"] == {"cache": True, "web": False}
    assert data["ok"] is False and data["exit_code"] == 2


def test_deploy_status_phase_and_wait(monkeypatch, tmp_path) -> None:
    import json

    monkeypatch.setenv("MANDALA_FLEET_STATE", str(tmp_path))
    from mandala_fleet import registry

    # Live pid + no host events yet → the batch-build play (play 1).
    run_id, path = registry.new_run_dir()
    registry.write_meta(path, {"run_id": run_id, "pid": 1, "limit": "cache,web"})
    data = _call(build_server(_inv()), "deploy_status", {"run_id": run_id})
    assert data["liveness"] == "running" and data["phase"] == "batch-build"

    # Dead pid + terminal host → done; wait_seconds returns immediately.
    run_id2, path2 = registry.new_run_dir()
    registry.write_meta(path2, {"run_id": run_id2, "pid": None, "limit": "cache"})
    with open(path2 / "cache.jsonl", "a", encoding="utf-8") as fh:
        fh.write(json.dumps(
            {"v": 1, "host": "cache", "plugin": "deploy",
             "event": "milestone", "milestone": "confirm"}) + "\n")
    data = _call(
        build_server(_inv()),
        "deploy_status",
        {"run_id": run_id2, "wait_seconds": 30},
    )
    assert data["liveness"] == "finished" and data["phase"] == "done"


def test_drift_summary_and_status_filter(monkeypatch, tmp_path) -> None:
    monkeypatch.setenv("MANDALA_FLEET_STATE", str(tmp_path))
    from mandala_fleet import drift as drift_mod

    monkeypatch.setattr(drift_mod, "repo_rev", lambda flake: "deadbeef")

    mcp = build_server(_inv())
    # Empty state dir → both deploy nodes judge no-snapshot.
    data = _call(mcp, "drift", {})
    assert data["summary"] == {"no-snapshot": 2} and data["total"] == 2

    # The filter narrows entries; summary stays whole-fleet.
    data = _call(mcp, "drift", {"statuses": ["drift"]})
    assert data["entries"] == [] and data["summary"] == {"no-snapshot": 2}
