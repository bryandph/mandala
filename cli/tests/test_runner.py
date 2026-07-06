"""Demux + state-machine tests over synthetic protocol-v1 events.

This is the headless half of the multi-host verification: a fan-out
where one host rolls back must flag that host without disturbing the
others' states."""

import json
import sys
import time
from pathlib import Path

from mandala_fleet.runner import COMMAND_LOG, CommandRun, EventTailer, HostState


def _write(path: Path, events: list[dict]) -> None:
    with open(path, "a", encoding="utf-8") as fh:
        for e in events:
            fh.write(json.dumps({"v": 1, "ts": 0.0, **e}) + "\n")


def _milestones(host: str, *names: str) -> list[dict]:
    return [
        {"host": host, "plugin": "deploy", "event": "milestone", "milestone": n}
        for n in names
    ]


def test_multi_host_demux_with_rollback(tmp_path: Path) -> None:
    # Build play events land in the first host's file (run_once).
    _write(tmp_path / "alpha.jsonl", [
        {"host": "alpha", "plugin": "build", "event": "status", "state": "start", "cmd": []},
        {"host": "alpha", "plugin": "build", "event": "progress",
         "built": 4, "finished": 4, "fetched": 9, "fetched_done": 9, "errors": 0,
         "current": "system-path"},
        {"host": "alpha", "plugin": "build", "event": "status", "state": "done", "rc": 0},
    ])
    _write(tmp_path / "alpha.jsonl", _milestones("alpha", "eval", "build", "copy", "activate", "wait", "confirm"))
    _write(tmp_path / "alpha.jsonl", [
        {"host": "alpha", "plugin": "deploy", "event": "status", "state": "done", "rc": 0},
    ])
    _write(tmp_path / "beta.jsonl", _milestones("beta", "eval", "copy", "activate", "rollback"))
    _write(tmp_path / "beta.jsonl", [
        {"host": "beta", "plugin": "deploy", "event": "line", "line": "magic rollback fired", "stream": "deploy"},
        {"host": "beta", "plugin": "deploy", "event": "status", "state": "done", "rc": 1},
    ])
    _write(tmp_path / "gamma.jsonl", _milestones("gamma", "eval", "copy"))

    tailer = EventTailer(tmp_path)
    tailer.poll()

    assert tailer.build.done and tailer.build.rc == 0
    assert tailer.build.finished == 4 and tailer.build.fetched == 9

    assert tailer.hosts["alpha"].state == HostState.CONFIRMED
    # The rolled-back host is flagged — and stays flagged despite rc=1.
    assert tailer.hosts["beta"].state == HostState.ROLLED_BACK
    assert "magic rollback fired" in tailer.hosts["beta"].lines
    # The others are untouched by beta's failure.
    assert tailer.hosts["gamma"].state == HostState.COPYING

    # Incremental: appended events advance only the touched host.
    _write(tmp_path / "gamma.jsonl", _milestones("gamma", "activate", "confirm"))
    tailer.poll()
    assert tailer.hosts["gamma"].state == HostState.CONFIRMED
    assert tailer.hosts["beta"].state == HostState.ROLLED_BACK


def test_nixlog_routes_to_sink_and_nowhere_else(tmp_path: Path) -> None:
    _write(tmp_path / "alpha.jsonl", [
        {"v": 2, "host": "alpha", "plugin": "build", "event": "nixlog",
         "line": '@nix {"action":"start","type":105}'},
    ])
    tailer = EventTailer(tmp_path)
    seen: list[str] = []
    tailer.nixlog_sink = seen.append
    tailer.poll()
    assert seen == ['@nix {"action":"start","type":105}']
    assert not tailer.build.lines  # nixlog never pollutes the line views
    assert "alpha" not in tailer.hosts


def test_failed_without_rollback_and_version_gate(tmp_path: Path) -> None:
    _write(tmp_path / "delta.jsonl", _milestones("delta", "eval", "copy"))
    _write(tmp_path / "delta.jsonl", [
        {"host": "delta", "plugin": "deploy", "event": "status", "state": "done", "rc": 2},
        # Future-versioned records must be ignored, not misread.
        {"v": 99, "host": "delta", "plugin": "deploy", "event": "milestone", "milestone": "confirm"},
    ])
    tailer = EventTailer(tmp_path)
    tailer.poll()
    assert tailer.hosts["delta"].state == HostState.FAILED


def _wait_for_rc(path: Path, timeout: float = 10.0) -> dict:
    """Poll meta.json until the reaper thread records the exit code."""
    from mandala_fleet import registry

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        meta = registry.read_meta(path)
        if "rc" in meta:
            return meta
        time.sleep(0.05)
    raise AssertionError("reaper never recorded rc")


def test_command_run_registers_tees_and_reaps(monkeypatch, tmp_path: Path) -> None:
    """The whole command-run contract: a registered run dir with kind+pid
    meta, output teed to output.log (surviving the launcher's client), and
    the exit code reaped into meta so observers can judge liveness."""
    monkeypatch.setenv("MANDALA_FLEET_STATE", str(tmp_path))
    run = CommandRun(
        argv=[sys.executable, "-c", "print('rebooting-ish'); raise SystemExit(3)"],
        kind="reboot",
        extra_meta={"limit": "web"},
    )
    run.start()
    assert run.launched and run.run_id

    meta = _wait_for_rc(run.run_dir)
    assert meta["kind"] == "reboot" and meta["limit"] == "web"
    assert meta["rc"] == 3
    log = (run.run_dir / COMMAND_LOG).read_text()
    assert "rebooting-ish" in log
    assert log.startswith("$ ")  # the argv header for post-mortems


def test_command_run_failed_launch_is_still_registered(monkeypatch, tmp_path: Path) -> None:
    monkeypatch.setenv("MANDALA_FLEET_STATE", str(tmp_path))
    run = CommandRun(argv=["/nonexistent/ans-reboot"], kind="reboot")
    run.start()
    assert not run.launched
    from mandala_fleet import registry

    meta = registry.read_meta(run.run_dir)
    assert meta["rc"] == 127 and "error" in meta
    assert "failed to launch" in (run.run_dir / COMMAND_LOG).read_text()
