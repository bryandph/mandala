"""Deploy-runner core: drive the fan-out playbook as a subprocess and
demux the per-host JSONL event streams into host state machines.

The playbook remains the engine — the --limit guard, throttle, and
deploy-rs magic rollback are never bypassed; this module only launches
it (with MANDALA_FLEET_EVENTS pointed at a private directory) and tails
the event files the mandala.fleet plugins append. The TUI renders from
these models without knowing which engine emitted the events — the
protocol (module_utils/events.py, v1) is the whole contract.

Headless-safe and frontend-agnostic: nothing here imports textual.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import time
from collections import deque
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path

SUPPORTED_EVENT_VERSION = 1

# Raw lines kept per host for the inspector view.
_MAX_LINES = 2000


class HostState(str, Enum):
    PENDING = "pending"
    EVALUATING = "evaluating"
    BUILDING = "building"
    COPYING = "copying"
    ACTIVATING = "activating"
    WAITING = "waiting"
    CONFIRMED = "confirmed"
    ROLLED_BACK = "rolled-back"
    FAILED = "failed"


_MILESTONE_STATE = {
    "eval": HostState.EVALUATING,
    "build": HostState.BUILDING,
    "copy": HostState.COPYING,
    "activate": HostState.ACTIVATING,
    "wait": HostState.WAITING,
    "confirm": HostState.CONFIRMED,
    "rollback": HostState.ROLLED_BACK,
}

# Terminal states are sticky: a late "done rc=1" must not unflag a
# rollback, and a confirmed host stays confirmed.
_TERMINAL = {HostState.CONFIRMED, HostState.ROLLED_BACK, HostState.FAILED}


@dataclass
class HostRun:
    name: str
    state: HostState = HostState.PENDING
    lines: deque = field(default_factory=lambda: deque(maxlen=_MAX_LINES))
    milestones: list = field(default_factory=list)
    rc: int | None = None

    def feed(self, event: dict) -> None:
        kind = event.get("event")
        if kind == "line":
            self.lines.append(event.get("line", ""))
        elif kind == "milestone":
            name = event.get("milestone")
            self.milestones.append(name)
            state = _MILESTONE_STATE.get(name)
            if state is not None and self.state not in _TERMINAL:
                self.state = state
            elif state in (HostState.ROLLED_BACK,):
                # rollback wins even over confirmed — deploy-rs can
                # confirm then roll back on the magic-rollback timeout.
                self.state = state
        elif kind == "status" and event.get("state") == "done":
            self.rc = event.get("rc")
            if self.rc not in (0, None) and self.state not in _TERMINAL:
                self.state = HostState.FAILED


@dataclass
class BuildModel:
    """The build pane's data: rendered straight from the build plugin's
    progress/line events (the sanctioned fallback for a nom pty)."""

    built: int = 0
    finished: int = 0
    fetched: int = 0
    fetched_done: int = 0
    errors: int = 0
    current: str = ""
    lines: deque = field(default_factory=lambda: deque(maxlen=200))
    done: bool = False
    rc: int | None = None

    def feed(self, event: dict) -> None:
        kind = event.get("event")
        if kind == "progress":
            for key in ("built", "finished", "fetched", "fetched_done", "errors"):
                if key in event:
                    setattr(self, key, event[key])
            self.current = event.get("current", self.current)
        elif kind == "line":
            self.lines.append(event.get("line", ""))
        elif kind == "status" and event.get("state") == "done":
            self.done = True
            self.rc = event.get("rc")


class EventTailer:
    """Incremental reader over an events directory: per-file offsets,
    version-gated records, routed to BuildModel / HostRun."""

    def __init__(self, directory: Path):
        self.directory = Path(directory)
        self._offsets: dict[Path, int] = {}
        self.hosts: dict[str, HostRun] = {}
        self.build = BuildModel()

    def host(self, name: str) -> HostRun:
        if name not in self.hosts:
            self.hosts[name] = HostRun(name=name)
        return self.hosts[name]

    def poll(self) -> int:
        """Consume newly appended events. Returns how many were read."""
        count = 0
        if not self.directory.is_dir():
            return 0
        for path in sorted(self.directory.glob("*.jsonl")):
            offset = self._offsets.get(path, 0)
            try:
                with open(path, "r", encoding="utf-8") as fh:
                    fh.seek(offset)
                    for line in fh:
                        if not line.endswith("\n"):
                            break  # partial write; re-read next poll
                        offset += len(line.encode("utf-8"))
                        count += 1
                        self._route(line)
            except OSError:
                continue
            self._offsets[path] = offset
        return count

    def _route(self, line: str) -> None:
        try:
            event = json.loads(line)
        except ValueError:
            return
        if event.get("v") != SUPPORTED_EVENT_VERSION:
            return
        if event.get("plugin") == "build":
            self.build.feed(event)
            return
        host = event.get("host")
        if host:
            self.host(host).feed(event)


@dataclass
class DeployRun:
    """One fan-out deploy: subprocess + event tailer.

    The default playbook is the operator's wrapper (playbooks/deploy.yaml
    under ansible_dir) when present — it pins the flake root — falling
    back to the collection FQCN for bare consumers.
    """

    limit: str
    dry_activate: bool = False
    throttle: int = 4
    ansible_dir: Path | None = None
    playbook: str | None = None
    events_dir: Path | None = None
    _proc: subprocess.Popen | None = None
    tailer: EventTailer | None = None
    output: deque = field(default_factory=lambda: deque(maxlen=4000))
    started_at: float | None = None

    def resolve_paths(self) -> None:
        if self.ansible_dir is None:
            self.ansible_dir = Path("ansible") if Path("ansible/ansible.cfg").is_file() else Path(".")
        if self.playbook is None:
            wrapper = self.ansible_dir / "playbooks/deploy.yaml"
            self.playbook = "playbooks/deploy.yaml" if wrapper.is_file() else "mandala.fleet.deploy"
        if self.events_dir is None:
            self.events_dir = Path(tempfile.mkdtemp(prefix="mandala-events-"))

    def start(self) -> None:
        self.resolve_paths()
        self.tailer = EventTailer(self.events_dir)
        env = dict(os.environ)
        env["MANDALA_FLEET_EVENTS"] = str(self.events_dir)
        env.setdefault("ANSIBLE_FORCE_COLOR", "0")
        # ansible block-buffers stdout when piped — without this, output
        # arrives in late multi-KB chunks and the view looks dead.
        env["PYTHONUNBUFFERED"] = "1"
        argv = [
            "ansible-playbook", str(self.playbook),
            "-l", self.limit,
            "-e", f"deploy_throttle={self.throttle}",
        ]
        if self.dry_activate:
            argv += ["-e", "deploy_dry_activate=true"]
        self.started_at = time.monotonic()
        self.output.append(f"$ {' '.join(argv)}  (cwd={self.ansible_dir}, events={self.events_dir})")
        try:
            self._proc = subprocess.Popen(
                argv,
                cwd=self.ansible_dir,
                env=env,
                # NEVER inherit the TUI's raw-mode stdin: an interactive
                # prompt (ssh, vault, become) would wedge the run silently.
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
            )
        except OSError as e:
            self.output.append(f"failed to launch {argv[0]}: {e}")
            return
        # Dedicated reader thread: readline() blocks safely HERE instead
        # of stalling the UI tick on a partial line.
        import threading

        def drain() -> None:
            assert self._proc is not None and self._proc.stdout is not None
            for line in self._proc.stdout:
                self.output.append(line.rstrip("\n"))

        threading.Thread(target=drain, daemon=True).start()

    def poll(self) -> None:
        if self.tailer is not None:
            self.tailer.poll()

    @property
    def returncode(self) -> int | None:
        return None if self._proc is None else self._proc.poll()

    @property
    def finished(self) -> bool:
        return self.returncode is not None

    def terminate(self) -> None:
        if self._proc is not None and self._proc.poll() is None:
            self._proc.terminate()
