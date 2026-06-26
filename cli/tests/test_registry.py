"""Run-registry tests: allocation, meta round-trip, prune retention, and
read-only attach/liveness — the cross-frontend monitoring contract.

State is redirected to a tmp dir via MANDALA_FLEET_STATE (the same env
state_dir() honors), so nothing here touches a real per-user registry.
"""

import json
from pathlib import Path

import pytest

from mandala_fleet import registry
from mandala_fleet.registry import RunLiveness


@pytest.fixture(autouse=True)
def _isolated_state(monkeypatch, tmp_path: Path):
    monkeypatch.setenv("MANDALA_FLEET_STATE", str(tmp_path))
    return tmp_path


def _write(path: Path, events: list[dict]) -> None:
    with open(path, "a", encoding="utf-8") as fh:
        for e in events:
            fh.write(json.dumps({"v": 1, "ts": 0.0, **e}) + "\n")


def _milestones(host: str, *names: str) -> list[dict]:
    return [
        {"host": host, "plugin": "deploy", "event": "milestone", "milestone": n}
        for n in names
    ]


def test_new_run_dir_and_meta_roundtrip(tmp_path: Path) -> None:
    run_id, path = registry.new_run_dir()
    assert path.is_dir()
    assert path == tmp_path / "runs" / run_id
    registry.write_meta(path, {"run_id": run_id, "limit": "@all", "pid": 1234})
    assert registry.read_meta(path)["limit"] == "@all"
    runs = registry.list_runs()
    assert [r.run_id for r in runs] == [run_id]
    assert runs[0].pid == 1234


def test_list_runs_sorts_most_recent_first(tmp_path: Path) -> None:
    base = tmp_path / "runs"
    base.mkdir(parents=True)
    # run-ids sort lexically by start time; later id == more recent.
    for rid in ("20260101T000000_000000-1", "20260102T000000_000000-1"):
        (base / rid).mkdir()
    assert [r.run_id for r in registry.list_runs()] == [
        "20260102T000000_000000-1",
        "20260101T000000_000000-1",
    ]


def test_prune_keeps_recent_and_spares_live_pids(tmp_path: Path, monkeypatch) -> None:
    base = tmp_path / "runs"
    base.mkdir(parents=True)
    # Six dead runs + one old live run; keep=2.
    ids = [f"20260101T0000{n:02d}_000000-1" for n in range(6)]
    for rid in ids:
        d = base / rid
        d.mkdir()
        registry.write_meta(d, {"pid": 999999})  # dead
    live = base / "20260101T000000_000000-9"  # oldest by id
    live.mkdir()
    registry.write_meta(live, {"pid": 4242})  # "alive"

    monkeypatch.setattr(registry, "pid_alive", lambda pid: pid == 4242)
    registry.prune(keep=2)

    survivors = {r.run_id for r in registry.list_runs()}
    # The two most-recent dead runs survive, plus the live one regardless of age.
    assert ids[5] in survivors and ids[4] in survivors
    assert ids[0] not in survivors and ids[3] not in survivors
    assert live.name in survivors


def test_open_run_liveness_running_then_terminal(tmp_path: Path, monkeypatch) -> None:
    run_id, path = registry.new_run_dir()
    registry.write_meta(path, {"run_id": run_id, "pid": 4242})
    _write(path / "alpha.jsonl", _milestones("alpha", "eval", "build", "copy", "activate", "confirm"))

    obs = registry.open_run(run_id)
    assert obs is not None
    obs.poll()
    # pid alive → RUNNING even though alpha already confirmed.
    monkeypatch.setattr(registry, "pid_alive", lambda pid: True)
    assert obs.liveness() is RunLiveness.RUNNING
    # pid gone, all hosts terminal & confirmed → FINISHED.
    monkeypatch.setattr(registry, "pid_alive", lambda pid: False)
    assert obs.liveness() is RunLiveness.FINISHED


def test_open_run_liveness_rollback_and_unknown(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setattr(registry, "pid_alive", lambda pid: False)

    rb_id, rb = registry.new_run_dir()
    registry.write_meta(rb, {"pid": 1})
    _write(rb / "beta.jsonl", _milestones("beta", "eval", "activate", "rollback"))
    obs_rb = registry.open_run(rb_id)
    obs_rb.poll()
    assert obs_rb.liveness() is RunLiveness.ROLLED_BACK

    # Dead pid, host stuck mid-flight (no terminal state) → UNKNOWN.
    unk_id, unk = registry.new_run_dir()
    registry.write_meta(unk, {"pid": 1})
    _write(unk / "gamma.jsonl", _milestones("gamma", "eval", "copy"))
    obs_unk = registry.open_run(unk_id)
    obs_unk.poll()
    assert obs_unk.liveness() is RunLiveness.UNKNOWN

    assert registry.open_run("nonesuch") is None
