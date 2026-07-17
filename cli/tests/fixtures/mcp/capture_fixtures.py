"""Capture golden JSON result shapes from the FastMCP Python server.

These fixtures are the PARITY ORACLE for the Rust MCP port (OpenSpec change
`mandala-rust-rewrite`, section 4): the Rust server's tool results must match
these shapes key-for-key. They are captured the same headless way the test
suite drives the server — FastMCP's in-memory `Client`, an injected aggregate
(no `nix eval`), and monkeypatched subprocess/launch points — so capture is
deterministic and needs neither the real fleet nor ansible/nix on PATH.

Regenerate:  (from flakes/mandala/, inside a python env with fastmcp)
    PYTHONPATH=cli/src python cli/tests/fixtures/mcp/capture_fixtures.py

Volatile fields (never assert on their VALUE in parity tests, only presence):
  run_id, log, events_dir, meta.pid, elapsed, ts, and any /nix/store or
  state-dir path. The capture normalizes the obvious ones to placeholders.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

FIXTURES = Path(__file__).resolve().parent
# Isolate all state (audit.jsonl, run registry) into a throwaway dir.
_STATE = tempfile.mkdtemp(prefix="mandala-fixtures-")
os.environ["MANDALA_FLEET_STATE"] = _STATE

import asyncio  # noqa: E402

from fastmcp import Client  # noqa: E402

from mandala_fleet.inventory import Inventory  # noqa: E402
from mandala_fleet.mcp import build_server  # noqa: E402


def _inv() -> Inventory:
    """The same injected aggregate the MCP test suite uses."""
    inv = Inventory(flake=".")
    inv.__dict__["aggregate"] = {
        "schemaVersion": 1,
        "members": {
            "web": {
                "platform": "metal",
                "architecture": "x86_64-linux",
                "category": "server",
                "role": "web",
                "tags": ["edge"],
            },
            "cache": {"platform": "metal", "architecture": "x86_64-linux"},
            "router": {"platform": "opnsense"},
        },
        "groups": {"k3s": ["cache", "web"], "gateway": ["router"]},
        "projections": {"deploy": {"nodes": ["cache", "web"]}},
    }
    return inv


def _call(mcp, name: str, args: dict):
    async def go():
        async with Client(mcp) as client:
            return await client.call_tool(name, args)

    return asyncio.run(go()).data


class _FakeProc:
    def __init__(self, stdout="", stderr="", returncode=0):
        self.stdout, self.stderr, self.returncode = stdout, stderr, returncode
        self.args = ["<captured>"]


def _norm(obj):
    """Blank out volatile identifiers so a git diff of the fixtures only moves
    when the SHAPE changes, not on every capture run."""
    if isinstance(obj, dict):
        out = {}
        for k, v in obj.items():
            if k in ("run_id", "events_dir"):
                out[k] = "<run-id>"
            elif k == "log":
                out[k] = "<state-dir>/runs/<run-id>/output.log"
            elif k == "elapsed":
                out[k] = 0.0
            elif k == "ts":
                out[k] = 0
            elif k == "pid":
                out[k] = None
            else:
                out[k] = _norm(v)
        return out
    if isinstance(obj, list):
        return [_norm(v) for v in obj]
    return obj


def dump(name: str, payload) -> None:
    (FIXTURES / f"{name}.json").write_text(
        json.dumps(_norm(payload), indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(f"  wrote {name}.json")


def capture_reads() -> None:
    mcp = build_server(_inv())
    dump("members.compact", _call(mcp, "members", {}))
    dump("members.full", _call(mcp, "members", {"full": True}))
    dump("groups.ok", _call(mcp, "groups", {}))
    dump("resolve.ok", _call(mcp, "resolve", {"selector": "all,!@gateway"}))


def capture_ping(monkeypatch_run) -> None:
    from mandala_fleet.mcp import server as server_mod

    server_mod.subprocess.run = monkeypatch_run(
        _FakeProc(
            stdout='web | SUCCESS => {"ping": "pong"}\n'
            "cache | UNREACHABLE! => {}\n",
            stderr="[ERROR]: remote: Counting objects: 100% (14/14)\n",
            returncode=4,
        )
    )
    dump("ping.mixed", _call(build_server(_inv()), "ping", {"selector": "@k3s"}))


def capture_host_eval() -> None:
    from mandala_fleet.mcp import server as server_mod

    # ok path with toplevel not requested (no eval).
    dump("host_eval.ok", _call(build_server(_inv()), "host_eval", {"member": "web"}))

    # eval_error path: force the toplevel eval to fail.
    def boom(flake, members):
        raise subprocess.CalledProcessError(
            1, ["nix", "eval", "--json", f".#nixosConfigurations.web..."],
            stderr="error: attribute 'web' missing\n",
        )

    orig = server_mod.drift_mod.eval_expected
    server_mod.drift_mod.eval_expected = boom
    try:
        dump(
            "host_eval.eval_error",
            _call(build_server(_inv()), "host_eval", {"member": "web", "toplevel": True}),
        )
    finally:
        server_mod.drift_mod.eval_expected = orig


def capture_drift() -> None:
    from mandala_fleet.mcp import server as server_mod

    orig_rev = server_mod.drift_mod.repo_rev
    server_mod.drift_mod.repo_rev = lambda flake: "deadbeef"
    try:
        mcp = build_server(_inv())
        # No snapshots, no eval → expected_source none, both nodes no-snapshot.
        dump("drift.ok", _call(mcp, "drift", {}))
        # Status filter narrows entries; summary stays whole-fleet.
        dump("drift.filtered", _call(mcp, "drift", {"statuses": ["drift"]}))

        # eval_error path: do_eval with a failing expected-toplevel eval.
        def boom(flake, nodes):
            raise subprocess.CalledProcessError(
                1, ["nix", "eval", "--json"], stderr="error: eval failed\n"
            )

        orig_eval = server_mod.drift_mod.eval_expected
        server_mod.drift_mod.eval_expected = boom
        try:
            dump("drift.eval_error", _call(build_server(_inv()), "drift", {"do_eval": True}))
        finally:
            server_mod.drift_mod.eval_expected = orig_eval
    finally:
        server_mod.drift_mod.repo_rev = orig_rev


def capture_reload() -> None:
    from mandala_fleet.mcp import server as server_mod

    fresh = Inventory(flake=".")
    fresh.__dict__["aggregate"] = {
        "schemaVersion": 1,
        "members": {"web": {}, "cache": {}, "router": {}, "newbie": {}},
        "groups": {"k3s": ["cache", "web"]},
        "projections": {"deploy": {"nodes": ["cache", "web"]}},
    }
    orig = server_mod.Inventory
    server_mod.Inventory = lambda flake: fresh
    try:
        dump("reload.ok", _call(build_server(_inv()), "reload", {}))
    finally:
        server_mod.Inventory = orig

    # unavailable path: getter-only wiring, no setter.
    inv = _inv()
    try:
        _call(build_server(lambda: inv), "reload", {})
    except Exception as e:  # noqa: BLE001
        dump("reload.unavailable_error", {"tool_error": str(e)})


def capture_actions() -> None:
    from mandala_fleet.mcp import server as server_mod
    from mandala_fleet import runner

    # deploy: refusal (real activation without confirm).
    dump(
        "deploy.refused",
        _call(build_server(_inv()), "deploy", {"selector": "@k3s", "dry_activate": False}),
    )

    # deploy: dry-run launch (stub start() so no subprocess).
    orig_start = runner.DeployRun.start
    runner.DeployRun.start = lambda self: self.resolve_paths()
    try:
        dump("deploy.dry_ok", _call(build_server(_inv()), "deploy", {"selector": "@k3s"}))
    finally:
        runner.DeployRun.start = orig_start

    # restart_service: refusal.
    dump(
        "restart_service.refused",
        _call(build_server(_inv()), "restart_service", {"selector": "@k3s", "unit": "k3s"}),
    )

    # restart_service: ok (monkeypatched ansible).
    server_mod.subprocess.run = lambda *a, **k: _FakeProc(
        stdout="cache | CHANGED => {}\nweb | FAILED! => {}\n", returncode=2
    )
    dump(
        "restart_service.partial",
        _call(
            build_server(_inv()),
            "restart_service",
            {"selector": "@k3s", "unit": "k3s", "confirm": "cache,web"},
        ),
    )

    # reboot: refusal.
    dump(
        "reboot.refused",
        _call(build_server(_inv()), "reboot", {"selector": "@k3s", "confirm": "web"}),
    )

    # reboot: ok launch (fake ans-reboot + fake Popen).
    import shutil

    orig_which = shutil.which
    shutil.which = lambda name: "/fake/ans-reboot" if name == "ans-reboot" else None

    class _FakePopen:
        pid = 54321

        def __init__(self, argv, **kwargs):
            self.argv = argv

        def wait(self):
            return 0

    orig_popen = runner.subprocess.Popen
    runner.subprocess.Popen = _FakePopen
    try:
        dump(
            "reboot.ok",
            _call(
                build_server(_inv()),
                "reboot",
                {"selector": "@k3s", "serial": "2", "confirm": "cache,web"},
            ),
        )
    finally:
        runner.subprocess.Popen = orig_popen
        shutil.which = orig_which

    # build: ok (fake CommandRun that writes a teed log with out-paths).
    class _FakeRun:
        launched = True

        def __init__(self, argv, kind, cwd=None, extra_meta=None):
            self.argv, self.kind = argv, kind
            self.extra_meta = extra_meta or {}
            self.run_id = None
            self.run_dir = None

        @property
        def log_path(self):
            return None if self.run_dir is None else self.run_dir / runner.COMMAND_LOG

        def start(self):
            from mandala_fleet import registry

            self.run_id, self.run_dir = registry.new_run_dir()
            self.log_path.write_text(
                "these derivations will be built:\n"
                "  /nix/store/x-toplevel.drv\n"
                "/nix/store/aaa-nixos-system-web\n"
            )
            registry.write_meta(
                self.run_dir,
                {
                    "run_id": self.run_id,
                    "kind": self.kind,
                    "pid": None,
                    "rc": 0,
                    **self.extra_meta,
                },
            )

    orig_cmdrun = runner.CommandRun
    runner.CommandRun = _FakeRun
    try:
        dump("build.ok", _call(build_server(_inv()), "build", {"selector": "@k3s"}))
    finally:
        runner.CommandRun = orig_cmdrun


def capture_deploy_status() -> None:
    from mandala_fleet import registry
    from mandala_fleet.runner import COMMAND_LOG

    # A reboot command run that failed.
    run_id, path = registry.new_run_dir()
    registry.write_meta(
        path, {"run_id": run_id, "kind": "reboot", "pid": None, "rc": 2, "limit": "web"}
    )
    (path / COMMAND_LOG).write_text("$ ans-reboot -l web\nfatal: boom\n")
    dump(
        "deploy_status.command",
        _call(build_server(_inv()), "deploy_status", {"run_id": run_id}),
    )

    # A deploy run with a host that reached confirmed.
    run_id2, path2 = registry.new_run_dir()
    registry.write_meta(path2, {"run_id": run_id2, "pid": None, "limit": "cache"})
    with open(path2 / "cache.jsonl", "a", encoding="utf-8") as fh:
        for m in ("eval", "build", "copy", "activate", "confirm"):
            fh.write(
                json.dumps(
                    {
                        "v": 1,
                        "host": "cache",
                        "plugin": "deploy",
                        "event": "milestone",
                        "milestone": m,
                    }
                )
                + "\n"
            )
    dump(
        "deploy_status.deploy",
        _call(build_server(_inv()), "deploy_status", {"run_id": run_id2}),
    )

    # The list form (most-recent runs).
    dump(
        "deploy_status.list",
        _call(build_server(_inv()), "deploy_status", {"limit": 5}),
    )


def _mp_run(proc):
    def run(*a, **k):
        return proc

    return run


def main() -> int:
    print(f"capturing MCP golden fixtures into {FIXTURES} (state={_STATE})")
    capture_reads()
    capture_ping(_mp_run)
    capture_host_eval()
    capture_drift()
    capture_reload()
    capture_actions()
    capture_deploy_status()
    print("done.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
