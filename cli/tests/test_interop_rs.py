"""Direction-B interop tests (fleet-state-formats): the RUST implementation
writes runs into the shared registry — via the `mandala-interop-helper`
binary driving the real `CommandRun`/`DeployRun` writers — and the PYTHON
implementation attaches to them live with `registry.open_run` /
`DeployRun.attach`.

Requires the built Rust artifacts: set `MANDALA_RS_INTEROP_BIN` to the
helper binary (the `mandala-interop` flake check does; locally:
`cargo build -p mandala-core --bin mandala-interop-helper` and point at
`target/debug/mandala-interop-helper`). Without it every test SKIPS, so
the `mandala-cli` package's pure-Python check phase is unchanged.

Payload commands are trivial (`sh -c 'echo …'`) — no ansible, nix, or
network. Direction A (Python writes, Rust reads) lives in
`crates/mandala-core/src/interop_tests.rs` over the checked-in fixtures.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import time
from pathlib import Path

import pytest

from mandala_fleet import drift, registry
from mandala_fleet.registry import RunLiveness
from mandala_fleet.runner import DeployRun, EventTailer, HostState

HELPER = os.environ.get("MANDALA_RS_INTEROP_BIN")
pytestmark = pytest.mark.skipif(
    not HELPER,
    reason="MANDALA_RS_INTEROP_BIN not set (built Rust interop helper unavailable)",
)

# Both implementations' run ids share this format (lexically sortable).
RUN_ID_RE = re.compile(r"^\d{8}T\d{6}_\d{6}-\d+$")


@pytest.fixture(autouse=True)
def _isolated_state(monkeypatch, tmp_path: Path) -> Path:
    """One private registry per test, shared with the helper subprocess
    through the inherited MANDALA_FLEET_STATE."""
    monkeypatch.setenv("MANDALA_FLEET_STATE", str(tmp_path))
    return tmp_path


def _helper(*args: str) -> dict:
    """Run the helper to completion and parse its run-info JSON line."""
    proc = subprocess.run(
        [HELPER, *args], capture_output=True, text=True, timeout=60, check=True
    )
    return json.loads(proc.stdout.splitlines()[0])


def _assert_meta_bytes(run_dir: Path) -> dict:
    """The Rust-written meta.json is byte-for-byte the canonical Python
    format (`json.dumps(indent=1, sort_keys=True)`)."""
    raw = (run_dir / "meta.json").read_text()
    meta = json.loads(raw)
    assert raw == json.dumps(meta, indent=1, sort_keys=True)
    return meta


def test_rust_command_run_live_then_reaped() -> None:
    """A Rust CommandRun is observable WHILE running (pid-alive liveness,
    teed output.log) and settles to the reaped rc once its reaper fires."""
    proc = subprocess.Popen(
        [HELPER, "command-run", "reboot", "sh", "-c", "echo mid-flight; sleep 2"],
        stdout=subprocess.PIPE,
        text=True,
    )
    try:
        info = json.loads(proc.stdout.readline())
        assert info["launched"] is True
        assert RUN_ID_RE.fullmatch(info["run_id"])

        obs = registry.open_run(info["run_id"])
        assert obs is not None
        assert obs.info.kind == "reboot"
        meta = _assert_meta_bytes(Path(info["run_dir"]))
        assert meta["argv"][0] == "sh"
        assert isinstance(meta["started_at"], float)
        # The recorded pid is genuinely alive: RUNNING, no fakes.
        assert obs.liveness() is RunLiveness.RUNNING

        # The teed log: argv header first, then the payload's output.
        log = Path(info["log"])
        for _ in range(100):
            if "mid-flight" in log.read_text():
                break
            time.sleep(0.05)
        text = log.read_text()
        assert text.startswith("$ sh -c")
        assert "mid-flight" in text
    finally:
        proc.wait(timeout=60)

    # The helper exits only after the reaper recorded rc: the long-attached
    # observer sees the exit code land (meta re-read inside liveness()).
    assert obs.liveness() is RunLiveness.FINISHED
    meta = _assert_meta_bytes(Path(info["run_dir"]))
    assert meta["rc"] == 0 and "finished_at" in meta


def test_rust_command_run_failure_reaps_nonzero_rc() -> None:
    info = _helper("command-run", "reboot", "sh", "-c", "echo boom 1>&2; exit 3")
    obs = registry.open_run(info["run_id"])
    assert obs is not None
    assert obs.liveness() is RunLiveness.FAILED
    meta = _assert_meta_bytes(Path(info["run_dir"]))
    assert meta["rc"] == 3
    assert "boom" in Path(info["log"]).read_text()


def test_rust_deploy_run_confirmed_hosts_attach() -> None:
    """Python's DeployRun.attach + EventTailer over a Rust-launched deploy
    whose events were written by a Rust emitter — including an unsupported
    v99 record that must be skipped with later records still consumed."""
    info = _helper("deploy-run", "alpha,beta", "deploy-ok")
    run = DeployRun.attach(info["run_id"])
    assert run is not None
    assert run.limit == "alpha,beta"
    nixlog: list[str] = []
    run.tailer.nixlog_sink = nixlog.append
    run.poll()

    for host in ("alpha", "beta"):
        h = run.tailer.hosts[host]
        assert h.state is HostState.CONFIRMED
        assert h.rc == 0
        assert h.milestones == ["eval", "build", "copy", "activate", "wait", "confirm"]
    assert not any("future-protocol-noise" in l for l in run.tailer.hosts["alpha"].lines)
    assert run.tailer.build.done and run.tailer.build.rc == 0
    assert len(nixlog) == 1 and nixlog[0].startswith("@nix ")

    assert run.finished  # the helper waited for the child, pid is gone
    assert run.returncode == 0
    obs = registry.open_run(info["run_id"])
    obs.poll()
    assert obs.liveness() is RunLiveness.FINISHED
    # Deploy meta is the exact Python field set — and byte-format.
    meta = _assert_meta_bytes(Path(info["events_dir"]))
    assert meta["limit"] == "alpha,beta"
    assert "argv" not in meta  # deploy meta records no argv (parity)


def test_rust_deploy_run_rollback_wins_and_confirmed_sticks() -> None:
    info = _helper("deploy-run", "gamma,delta", "deploy-rollback")
    run = DeployRun.attach(info["run_id"])
    assert run is not None
    run.poll()
    assert run.tailer.hosts["gamma"].state is HostState.ROLLED_BACK
    assert run.tailer.hosts["delta"].state is HostState.CONFIRMED  # sticky
    assert run.tailer.hosts["delta"].rc == 1
    assert run.finished and run.returncode == 1

    obs = registry.open_run(info["run_id"])
    obs.poll()
    assert obs.liveness() is RunLiveness.ROLLED_BACK


def test_rust_deploy_run_batch_build_death_is_failed() -> None:
    info = _helper("deploy-run", "epsilon", "build-death")
    obs = registry.open_run(info["run_id"])
    assert obs is not None
    obs.poll()
    assert not obs.tailer.hosts  # died before any host event existed
    assert obs.liveness() is RunLiveness.FAILED


def test_rust_expected_cache_bytes_match_python(tmp_path: Path) -> None:
    rev = "0123456789abcdef0123456789abcdef01234567"
    toplevels = {
        "alpha": "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-nixos-system-alpha",
        "beta": "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-nixos-system-beta",
    }
    rs_dir, py_dir = tmp_path / "rs", tmp_path / "py"
    subprocess.run(  # prints nothing — no info line to parse
        [HELPER, "save-expected", str(rs_dir), rev]
        + [f"{h}={t}" for h, t in toplevels.items()],
        check=True,
        timeout=60,
    )
    assert drift.load_expected(rs_dir) == (rev, toplevels)
    drift.save_expected(rev, toplevels, py_dir)
    assert (rs_dir / ".expected.json").read_bytes() == (
        py_dir / ".expected.json"
    ).read_bytes()


def test_run_ids_interleave_and_prune_spares_rust_live_run(tmp_path: Path) -> None:
    """Rust- and Python-allocated run ids share one sort order, and a
    Python prune never drops the Rust implementation's live run."""
    proc = subprocess.Popen(  # the Rust live run — allocated FIRST (oldest)
        [HELPER, "command-run", "build", "sh", "-c", "sleep 3"],
        stdout=subprocess.PIPE,
        text=True,
    )
    try:
        rust_info = json.loads(proc.stdout.readline())
        py_ids = []
        for _ in range(3):
            time.sleep(0.01)  # distinct microsecond timestamps
            run_id, path = registry.new_run_dir()
            registry.write_meta(path, {"run_id": run_id, "pid": None})
            py_ids.append(run_id)

        ids = [r.run_id for r in registry.list_runs()]
        assert ids == sorted([rust_info["run_id"], *py_ids], reverse=True)
        assert ids[-1] == rust_info["run_id"]  # oldest

        # keep=1 would prune the oldest dead runs — the Rust run's pid is
        # genuinely alive, so it survives regardless of age.
        registry.prune(keep=1)
        survivors = {r.run_id for r in registry.list_runs()}
        assert rust_info["run_id"] in survivors
        assert survivors == {rust_info["run_id"], py_ids[-1]}
    finally:
        proc.wait(timeout=60)


def test_python_tailer_survives_rust_partial_line(tmp_path: Path) -> None:
    """A torn trailing line (a mid-write snapshot of the Rust emitter's
    file) is not consumed until completed — parity of the fixture-based
    direction-A test, exercised over Rust-emitted bytes."""
    events = tmp_path / "events"
    events.mkdir()
    env = dict(os.environ, MANDALA_FLEET_EVENTS=str(events))
    subprocess.run([HELPER, "emit-events", "deploy-ok"], check=True, timeout=60, env=env)

    # Tear the final record the way an in-flight write would look.
    beta = events / "beta.jsonl"
    raw = beta.read_bytes()
    cut = raw.rstrip(b"\n").rfind(b"\n") + 1 + 20
    beta.write_bytes(raw[:cut])

    tailer = EventTailer(events)
    tailer.poll()
    assert tailer.hosts["beta"].rc is None  # the torn `done` not consumed
    assert tailer.hosts["beta"].state is HostState.CONFIRMED

    with open(beta, "ab") as fh:
        fh.write(raw[cut:])
    assert tailer.poll() == 1
    assert tailer.hosts["beta"].rc == 0
