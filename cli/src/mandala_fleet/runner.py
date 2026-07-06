"""Deploy-runner core: drive the fan-out playbook as a subprocess and
demux the per-host JSONL event streams into host state machines.

The playbook remains the engine — the --limit guard, throttle, and
deploy-rs magic rollback are never bypassed; this module only launches
it (with MANDALA_FLEET_EVENTS pointed at a run directory) and tails the
event files the mandala.fleet plugins append. The TUI renders from these
models without knowing which engine emitted the events — the protocol
(module_utils/events.py, v1) is the whole contract.

By default a run launches into the discoverable run registry (registry.py)
rather than a private mkdtemp, so a second TUI, the CLI, or the fleet MCP
server can attach to an in-flight or recent run and render it from the same
event streams. Passing an explicit `events_dir` keeps the old private
behavior (and is what the tests use).

Headless-safe and frontend-agnostic: nothing here imports textual.
"""

from __future__ import annotations

import json
import os
import subprocess
import time
from collections import deque
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path

# v2 = v1 + the `nixlog` event type (verbatim internal-json for nom).
SUPPORTED_EVENT_VERSIONS = frozenset({1, 2})

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
        # Attach a callable BEFORE polling starts to receive every raw
        # internal-json line live (nom food); None drops them.
        self.nixlog_sink = None

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
        if event.get("v") not in SUPPORTED_EVENT_VERSIONS:
            return
        if event.get("event") == "nixlog":
            if self.nixlog_sink is not None:
                self.nixlog_sink(event.get("line", ""))
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
    run_id: str | None = None
    _proc: subprocess.Popen | None = None
    tailer: EventTailer | None = None
    output: deque = field(default_factory=lambda: deque(maxlen=4000))
    started_at: float | None = None
    # Observer mode: attached to a run another process launched (e.g. a
    # Claude-triggered deploy). No subprocess is owned; liveness/returncode
    # are derived from the registry pid + sticky terminal host states.
    _attached: bool = False
    _meta_pid: int | None = None

    @classmethod
    def attach(cls, run_id: str) -> DeployRun | None:
        """Read-only attach to an already-launched registry run: tail its
        events without owning a subprocess, so a run started by another
        frontend can be rendered identically. None if the run is gone."""
        from . import registry

        obs = registry.open_run(run_id)
        if obs is None:
            return None
        meta = obs.info.meta
        run = cls(
            limit=meta.get("limit", ""),
            dry_activate=bool(meta.get("dry_activate", False)),
            events_dir=obs.info.path,
            run_id=run_id,
        )
        run.tailer = obs.tailer
        run._attached = True
        run._meta_pid = meta.get("pid")
        run.started_at = time.monotonic()  # for elapsed display only
        return run

    def resolve_paths(self) -> None:
        if self.ansible_dir is None:
            self.ansible_dir = Path("ansible") if Path("ansible/ansible.cfg").is_file() else Path(".")
        if self.playbook is None:
            wrapper = self.ansible_dir / "playbooks/deploy.yaml"
            self.playbook = "playbooks/deploy.yaml" if wrapper.is_file() else "mandala.fleet.deploy"
        if self.events_dir is None:
            # Default into the discoverable registry; lazy import keeps the
            # registry's `from .runner import ...` from cycling at load time.
            from . import registry

            self.run_id, self.events_dir = registry.new_run_dir()

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
        # Record the run so other frontends (a second TUI, the MCP server)
        # can discover it and tail the same events. Keyed on the live
        # subprocess pid so an observer can judge liveness without owning it.
        try:
            from . import registry

            registry.write_meta(self.events_dir, {
                "run_id": self.run_id,
                "limit": self.limit,
                "dry_activate": self.dry_activate,
                "throttle": self.throttle,
                "playbook": str(self.playbook),
                "pid": self._proc.pid,
                "started_at": time.time(),
            })
        except OSError:
            pass  # a registry write must never sink the run itself
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
        if self._attached:
            if not self.finished:
                return None
            # Derive an exit code from the sticky terminal host states: any
            # failed/rolled-back host means the run did not cleanly succeed.
            states = {h.state for h in (self.tailer.hosts.values() if self.tailer else [])}
            return 1 if (states & {HostState.FAILED, HostState.ROLLED_BACK}) else 0
        return None if self._proc is None else self._proc.poll()

    @property
    def finished(self) -> bool:
        if self._attached:
            from . import registry

            return not registry.pid_alive(self._meta_pid)
        return self.returncode is not None

    def terminate(self) -> None:
        if self._attached:
            return  # an observer never owns the subprocess
        if self._proc is not None and self._proc.poll() is None:
            self._proc.terminate()


# Command-run output file: sits beside the event JSONLs in the run dir
# (EventTailer globs *.jsonl, so it never routes this).
COMMAND_LOG = "output.log"


@dataclass
class CommandRun:
    """A registered background command (reboot playbook, …): the argv runs
    detached with stdout+stderr teed to `output.log` under a registry run
    dir, `meta.json` carries kind + pid so any frontend can discover and
    tail it, and a reaper thread records the exit code into meta when the
    subprocess exits. The launching client (an MCP call, a TUI screen)
    can therefore vanish — timeout, quit — without orphaning the run
    unobservably or losing its output."""

    argv: list[str]
    kind: str
    cwd: Path | None = None
    extra_meta: dict = field(default_factory=dict)
    run_id: str | None = None
    run_dir: Path | None = None
    _proc: subprocess.Popen | None = None

    @property
    def log_path(self) -> Path | None:
        return None if self.run_dir is None else self.run_dir / COMMAND_LOG

    def start(self) -> None:
        from . import registry

        self.run_id, self.run_dir = registry.new_run_dir()
        env = dict(os.environ)
        env.setdefault("ANSIBLE_FORCE_COLOR", "0")
        env["PYTHONUNBUFFERED"] = "1"
        log = open(self.log_path, "a", encoding="utf-8")
        log.write(f"$ {' '.join(self.argv)}  (cwd={self.cwd or '.'})\n")
        log.flush()
        try:
            self._proc = subprocess.Popen(
                self.argv,
                cwd=self.cwd,
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=subprocess.STDOUT,
            )
        except OSError as e:
            log.write(f"failed to launch {self.argv[0]}: {e}\n")
            log.close()
            registry.write_meta(self.run_dir, {
                "run_id": self.run_id,
                "kind": self.kind,
                "pid": None,
                "rc": 127,
                "error": str(e),
                "started_at": time.time(),
                **self.extra_meta,
            })
            return
        log.close()  # the subprocess holds its own fd now
        registry.write_meta(self.run_dir, {
            "run_id": self.run_id,
            "kind": self.kind,
            "pid": self._proc.pid,
            "argv": self.argv,
            "started_at": time.time(),
            **self.extra_meta,
        })

        # Reap in the background: liveness flips from pid-alive to the
        # recorded rc, so an observer's judgement survives the launcher's
        # client disappearing (only the launcher PROCESS dying loses it).
        import threading

        def reap() -> None:
            rc = self._proc.wait()
            try:
                registry.update_meta(self.run_dir, rc=rc, finished_at=time.time())
            except OSError:
                pass  # the run dir may have been pruned underneath us

        threading.Thread(target=reap, daemon=True).start()

    @property
    def launched(self) -> bool:
        return self._proc is not None
