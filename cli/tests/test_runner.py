"""Demux + state-machine tests over synthetic protocol-v1 events.

This is the headless half of the multi-host verification: a fan-out
where one host rolls back must flag that host without disturbing the
others' states."""

import json
from pathlib import Path

from mandala_fleet.runner import EventTailer, HostState


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
