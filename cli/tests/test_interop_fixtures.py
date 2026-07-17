"""The Python half of the direction-A interop gate (fleet-state-formats).

The checked-in fixture tree `fixtures/interop/state/` was written by the
Python implementation's real code paths (see `fixtures/interop/generate.py`
— the provenance; regenerate only through it). The Rust implementation
reads it in `crates/mandala-core/src/interop_tests.rs`; THIS file asserts
the Python reading of the exact same bytes, so both implementations are
pinned to one judgement over one tree — a divergence in either reader
fails one of the two suites before the change ships.

Expectations here mirror the Rust test file case-for-case; if the tree's
shape changes, update the generator and both test files together.
"""

from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path

import pytest

from mandala_fleet import drift, registry
from mandala_fleet.drift import DriftStatus
from mandala_fleet.registry import RunLiveness
from mandala_fleet.runner import DeployRun, EventTailer, HostState

STATE = Path(__file__).resolve().parent / "fixtures/interop/state"

# Fixture run ids (suffix = the fake pid recorded in meta; liveness is
# faked per pid, 555555 being the one "live" foreign pid).
RUN_A = "20260101T000000_000000-1001"  # deploy, all confirmed
RUN_B = "20260101T000100_000000-1002"  # rollback + sticky confirmed
RUN_C = "20260101T000200_000000-1003"  # batch-build death
RUN_D = "20260101T000300_000000-1004"  # reboot, reaped rc=3
RUN_E = "20260101T000400_000000-1005"  # build, pid "alive"
LIVE_PID = 555555

NOW = datetime(2026, 1, 2, tzinfo=timezone.utc)


@pytest.fixture(autouse=True)
def _fixture_state(monkeypatch):
    """Point the registry at the READ-ONLY fixture tree (tests that mutate
    — pruning — copy it out first)."""
    monkeypatch.setenv("MANDALA_FLEET_STATE", str(STATE))


def test_meta_bytes_are_the_canonical_python_format() -> None:
    """Every fixture meta.json is exactly `json.dumps(indent=1,
    sort_keys=True)` of its own content — the byte format the Rust writer
    reproduces (asserted from the Rust side by its round-trip test)."""
    for run in (RUN_A, RUN_B, RUN_C, RUN_D, RUN_E):
        raw = (STATE / "runs" / run / "meta.json").read_text()
        assert raw == json.dumps(json.loads(raw), indent=1, sort_keys=True)


def test_expected_cache_reads_and_rewrites_byte_identical(tmp_path: Path) -> None:
    rev, toplevels = drift.load_expected(STATE)
    assert rev == "0123456789abcdef0123456789abcdef01234567"
    assert len(toplevels) == 5
    assert toplevels["alpha"].endswith("-nixos-system-alpha-26.05")

    drift.save_expected(rev, toplevels, tmp_path)
    assert (tmp_path / ".expected.json").read_bytes() == (
        STATE / ".expected.json"
    ).read_bytes()


def test_snapshots_judge_identically() -> None:
    snapshots = drift.read_snapshots(STATE)
    assert ".expected" not in snapshots  # the cache dot-file is not a snapshot
    _, expected = drift.load_expected(STATE)
    nodes = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"]
    entries = drift.compare(nodes, snapshots, expected, now=NOW)
    assert {e.host: e.status for e in entries} == {
        "alpha": DriftStatus.IN_SYNC,
        "beta": DriftStatus.DRIFT,
        "gamma": DriftStatus.REBOOT_PENDING,
        "delta": DriftStatus.ACTIVATED,  # params differ only in whitespace
        "epsilon": DriftStatus.UNREACHABLE,
        "zeta": DriftStatus.INCOMPLETE,
        "eta": DriftStatus.NO_SNAPSHOT,
        "theta": DriftStatus.STALE,
    }


def test_deploy_run_reads_identically() -> None:
    obs = registry.open_run(RUN_A)
    assert obs is not None
    assert obs.info.kind == "deploy"
    assert obs.info.meta["limit"] == "alpha,beta"

    nixlog: list[str] = []
    obs.tailer.nixlog_sink = nixlog.append
    obs.poll()

    alpha = obs.tailer.hosts["alpha"]
    assert alpha.state is HostState.CONFIRMED
    assert alpha.rc == 0
    assert alpha.milestones == ["eval", "build", "copy", "activate", "wait", "confirm"]

    beta = obs.tailer.hosts["beta"]
    assert beta.state is HostState.CONFIRMED
    assert beta.rc == 0
    # The torn final line must not surface.
    assert not any("tail-marker-line" in l for l in beta.lines)

    assert len(nixlog) == 1 and nixlog[0].startswith("@nix ")

    build = obs.tailer.build
    assert (build.built, build.finished, build.fetched, build.fetched_done) == (3, 2, 5, 5)
    assert build.current == "nixos-system-alpha"
    assert build.done and build.rc == 0


def test_torn_line_completes_on_next_poll(tmp_path: Path) -> None:
    src = STATE / "runs" / RUN_A
    (tmp_path / "beta.jsonl").write_bytes((src / "beta.jsonl").read_bytes())

    tailer = EventTailer(tmp_path)
    tailer.poll()
    assert not any("tail-marker-line" in l for l in tailer.hosts["beta"].lines)

    # The writer finishes its interrupted write; the tailer resumes from
    # the un-advanced offset and consumes exactly the completed record.
    with open(tmp_path / "beta.jsonl", "ab") as fh:
        fh.write((src / "beta.jsonl.tail").read_bytes())
    assert tailer.poll() == 1
    assert any(l == "tail-marker-line" for l in tailer.hosts["beta"].lines)


def test_liveness_agreement(monkeypatch) -> None:
    def case(run_id: str, alive: bool, want: RunLiveness):
        monkeypatch.setattr(registry, "pid_alive", lambda pid: alive)
        obs = registry.open_run(run_id)
        assert obs is not None
        obs.poll()
        assert obs.liveness() is want, f"run {run_id} (alive={alive})"
        return obs

    case(RUN_A, True, RunLiveness.RUNNING)
    case(RUN_A, False, RunLiveness.FINISHED)
    obs_b = case(RUN_B, False, RunLiveness.ROLLED_BACK)
    # Sticky confirmed + the record AFTER the unsupported v99 one consumed.
    assert obs_b.tailer.hosts["delta"].state is HostState.CONFIRMED
    assert obs_b.tailer.hosts["delta"].rc == 1
    assert not any("future-protocol-noise" in l for l in obs_b.tailer.hosts["delta"].lines)
    case(RUN_C, False, RunLiveness.FAILED)  # batch-build death
    obs_d = case(RUN_D, False, RunLiveness.FAILED)  # reaped rc=3
    assert obs_d.info.kind == "reboot"
    case(RUN_E, True, RunLiveness.RUNNING)
    case(RUN_E, False, RunLiveness.UNKNOWN)  # dead pid, no rc, no events


def test_deploy_run_attach_over_fixture_runs(monkeypatch) -> None:
    monkeypatch.setattr(registry, "pid_alive", lambda pid: False)

    ok = DeployRun.attach(RUN_A)
    assert ok is not None and ok.limit == "alpha,beta"
    ok.poll()
    assert ok.finished and ok.returncode == 0

    rb = DeployRun.attach(RUN_B)
    assert rb is not None
    rb.poll()
    assert rb.finished and rb.returncode == 1  # a rolled-back host must fail


def test_prune_spares_foreign_live_run(monkeypatch, tmp_path: Path) -> None:
    import shutil

    shutil.copytree(STATE / "runs", tmp_path / "runs")
    # copytree preserves source modes; when the fixtures come from the nix
    # store (the flake check) the copied dirs are read-only and rmtree's
    # ignore_errors would silently prune nothing — make the copy writable.
    for p in [tmp_path / "runs", *(tmp_path / "runs").rglob("*")]:
        p.chmod(p.stat().st_mode | 0o200)
    monkeypatch.setenv("MANDALA_FLEET_STATE", str(tmp_path))
    monkeypatch.setattr(registry, "pid_alive", lambda pid: pid == LIVE_PID)

    registry.prune(keep=1)
    assert [r.run_id for r in registry.list_runs()] == [RUN_E, RUN_D]


def test_run_ids_sort_most_recent_first() -> None:
    assert [r.run_id for r in registry.list_runs()] == [RUN_E, RUN_D, RUN_C, RUN_B, RUN_A]
