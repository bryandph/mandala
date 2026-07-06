"""Discoverable deploy-run registry: a per-user directory of recent runs
so any frontend — a second TUI, the CLI, the fleet MCP server — can find
an in-flight or recent run and tail its event streams.

Each run owns a directory under `state_dir()/runs/<run-id>/` holding its
per-host event JSONLs (the same files EventTailer globs) plus a small
`meta.json` (limit, dry_activate, throttle, pid, started_at, playbook).
Reusing `drift.state_dir()` keeps one per-user state root — persistent,
not a world-writable /tmp parent — and the run-id sorts lexically by
start time, so listing is a sorted glob.

Everything an observer does here is read-only: it opens an existing run
dir, tails its files, and derives liveness from the recorded pid plus the
protocol's sticky terminal host states. It never owns the subprocess —
the launching frontend remains the parent, and a deploy launched in one
terminal is observable from another exactly because the run dir is shared.
"""

from __future__ import annotations

import json
import os
import shutil
from dataclasses import dataclass
from datetime import datetime, timezone
from enum import Enum
from pathlib import Path

from .drift import state_dir
from .runner import _TERMINAL, EventTailer, HostState

# Keep the N most-recent run dirs; older ones are pruned when a new run
# is allocated. A run whose recorded pid is still alive is NEVER pruned,
# regardless of N. Override via MANDALA_FLEET_RUN_KEEP.
DEFAULT_KEEP = 20

_META = "meta.json"


def runs_dir() -> Path:
    """The run-registry root, resolved at call time (mirrors state_dir)."""
    return state_dir() / "runs"


def _keep() -> int:
    raw = os.environ.get("MANDALA_FLEET_RUN_KEEP")
    if raw:
        try:
            return max(1, int(raw))
        except ValueError:
            pass
    return DEFAULT_KEEP


def _now_id() -> str:
    # Lexically sortable by start time; microseconds + pid disambiguate
    # two runs launched in the same second.
    ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S_%f")
    return f"{ts}-{os.getpid()}"


def pid_alive(pid: int | None) -> bool:
    """Whether a recorded run pid is still running. Signal 0 probes
    existence without delivering anything."""
    if not pid:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True  # exists, owned by another user
    return True


class RunLiveness(str, Enum):
    RUNNING = "running"  # recorded pid alive, no whole-run terminal yet
    FINISHED = "finished"  # pid gone, every host terminal, none failed
    FAILED = "failed"  # pid gone, a host failed
    ROLLED_BACK = "rolled-back"  # pid gone, a host rolled back
    UNKNOWN = "unknown"  # pid gone, no terminal state reached


@dataclass
class RunInfo:
    run_id: str
    path: Path
    meta: dict

    @property
    def pid(self) -> int | None:
        return self.meta.get("pid")

    @property
    def kind(self) -> str:
        """What launched into this run dir: `deploy` (event-streaming
        playbook) or a command kind (`reboot`, …) whose only stream is
        its teed output.log."""
        return self.meta.get("kind", "deploy")


def read_meta(path: Path) -> dict:
    try:
        return json.loads((Path(path) / _META).read_text())
    except (OSError, ValueError):
        return {}


def write_meta(path: Path, meta: dict) -> None:
    # Atomic replace: the launcher REWRITES meta (the reaper recording rc)
    # while observers poll it — a partial read must never surface as an
    # empty meta / wrong kind.
    target = Path(path) / _META
    tmp = target.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(meta, indent=1, sort_keys=True))
    os.replace(tmp, target)


def update_meta(path: Path, **fields) -> None:
    """Merge fields into an existing meta.json (read-modify-write) — how
    a launcher records the exit code once its command run finishes."""
    write_meta(path, {**read_meta(path), **fields})


def list_runs() -> list[RunInfo]:
    """Recent runs, most-recent first (the run-id sorts by start time)."""
    base = runs_dir()
    if not base.is_dir():
        return []
    runs = [
        RunInfo(run_id=d.name, path=d, meta=read_meta(d))
        for d in base.iterdir()
        if d.is_dir()
    ]
    runs.sort(key=lambda r: r.run_id, reverse=True)
    return runs


def prune(keep: int | None = None) -> None:
    """Drop all but the most-recent `keep` run dirs; never drop a run
    whose recorded pid is still alive (an observer may be attached)."""
    keep = _keep() if keep is None else keep
    survivors = 0
    for info in list_runs():  # most-recent first
        if pid_alive(info.pid):
            continue  # live runs are kept and don't count against the cap
        survivors += 1
        if survivors > keep:
            shutil.rmtree(info.path, ignore_errors=True)


def new_run_dir() -> tuple[str, Path]:
    """Prune stale runs, then allocate a fresh registered run directory."""
    prune()
    base = runs_dir()
    base.mkdir(parents=True, exist_ok=True)
    run_id = _now_id()
    path = base / run_id
    path.mkdir(parents=True, exist_ok=True)
    return run_id, path


@dataclass
class ObservedRun:
    """Read-only attachment to an existing run dir: tail its events and
    judge liveness without owning the subprocess."""

    info: RunInfo
    tailer: EventTailer

    def poll(self) -> int:
        return self.tailer.poll()

    def liveness(self) -> RunLiveness:
        # Meta is written and UPDATED by the launcher (pid at start, rc when
        # the reaper fires) — re-read it so a long-attached observer sees
        # the exit code land instead of judging from a stale snapshot.
        self.info.meta = read_meta(self.info.path)
        # A live pid means the fan-out is still going, even if one host has
        # already reached a sticky terminal state.
        if pid_alive(self.info.pid):
            return RunLiveness.RUNNING
        # A command run (reboot, …) has no host event streams; its launcher
        # records the exit code into meta when the subprocess exits.
        rc = self.info.meta.get("rc")
        if rc is not None:
            return RunLiveness.FINISHED if rc == 0 else RunLiveness.FAILED
        states = {h.state for h in self.tailer.hosts.values()}
        if HostState.ROLLED_BACK in states:
            return RunLiveness.ROLLED_BACK
        if HostState.FAILED in states:
            return RunLiveness.FAILED
        if states and states <= _TERMINAL:
            return RunLiveness.FINISHED
        # A deploy that died in the batch build never emitted host events —
        # judge from the build stream so it lands failed, not unknown.
        build = self.tailer.build
        if build.done and build.rc not in (0, None):
            return RunLiveness.FAILED
        return RunLiveness.UNKNOWN


def open_run(run_id: str) -> ObservedRun | None:
    """Attach read-only to a registered run by id (None if it's gone)."""
    path = runs_dir() / run_id
    if not path.is_dir():
        return None
    info = RunInfo(run_id=run_id, path=path, meta=read_meta(path))
    return ObservedRun(info=info, tailer=EventTailer(path))
