# -*- coding: utf-8 -*-
"""Subprocess runner for mandala.fleet.{build,deploy} with dual-mode output.

Two execution paths:

  TTY (interactive):   nom owns /dev/tty (live tree). We tee builder.stderr
                       into nom's stdin AND into our internal-json parser
                       (for summary + failure-log capture).

  Flat (non-TTY):      no nom. We parse Nix's `--log-format internal-json`
                       envelopes ourselves and emit one summary line per
                       ~2s through ansible's display.

`run_build()` is the build entry point (used by both action plugins for the
build phase). `run_command_streaming()` wraps deploy-rs's text status output
since deploy-rs does not emit internal-json.

Both runners take an optional `events` Emitter (module_utils.events) — the
opt-in JSONL channel. With `events=None` (the channel unset) every code
path is identical to the channel-less behavior; with it set,
`run_command_streaming` uses the PTY path unconditionally so every line is
observable (a porcelain frontend owns the terminal anyway).
"""

import json
import os
import shlex
import subprocess
import threading
import time


# Nix internal-json action types we care about. From nix/src/libutil/logging.hh.
_ACT_COPY_PATH = 100
_ACT_COPY_PATHS = 103
_ACT_BUILD = 105
_RES_BUILD_LOG_LINE = 101

_NIX_JSON_PREFIX = "@nix "
_SUMMARY_INTERVAL_S = 2.0
_MAX_BUILD_LOG_LINES = 500
_MAX_FAILURE_LOG_LINES = 200
_NIX_HASH_CHARS = frozenset("0123456789abcdfghijklmnpqrsvwxyz")


def quote_argv(argv):
    return " ".join(shlex.quote(str(a)) for a in argv)


def resolve_output_mode(mode):
    """Resolve `auto|tty|flat` into a concrete mode by probing /dev/tty."""
    if mode in ("tty", "flat"):
        return mode
    try:
        fd = os.open("/dev/tty", os.O_WRONLY | os.O_NOCTTY)
    except OSError:
        return "flat"
    try:
        return "tty" if os.isatty(fd) else "flat"
    finally:
        os.close(fd)


class _Tracker:
    """In-memory accumulator for nix internal-json events.

    Thread-safe writes; snapshot reads under the same lock.
    """

    def __init__(self):
        self._lock = threading.Lock()
        self.started_builds = 0
        self.finished_builds = 0
        self.started_fetches = 0
        self.finished_fetches = 0
        self.errors = 0
        self.running_builds = {}  # id -> name
        self.last_finished_name = None
        self.build_logs = {}      # id -> list[str]
        self.error_msgs = []
        self.start_t = time.monotonic()

    def feed_line(self, line):
        """Parse one nix stderr line. Returns True if state changed."""
        if not line.startswith(_NIX_JSON_PREFIX):
            return False
        try:
            ev = json.loads(line[len(_NIX_JSON_PREFIX):])
        except (ValueError, json.JSONDecodeError):
            return False

        action = ev.get("action")
        if action == "start":
            t = ev.get("type")
            ev_id = ev.get("id")
            fields = ev.get("fields") or []
            with self._lock:
                if t == _ACT_BUILD:
                    name = self._derivation_name(fields[0] if fields else "")
                    self.running_builds[ev_id] = name
                    self.started_builds += 1
                    return True
                if t in (_ACT_COPY_PATH, _ACT_COPY_PATHS):
                    self.started_fetches += 1
                    return True
        elif action == "stop":
            ev_id = ev.get("id")
            with self._lock:
                if ev_id in self.running_builds:
                    self.last_finished_name = self.running_builds.pop(ev_id)
                    self.finished_builds += 1
                    return True
                if self.started_fetches > self.finished_fetches:
                    self.finished_fetches += 1
                    return True
        elif action == "result":
            if ev.get("type") == _RES_BUILD_LOG_LINE:
                parent_id = ev.get("id")
                fields = ev.get("fields") or []
                if parent_id is not None and fields:
                    with self._lock:
                        bucket = self.build_logs.setdefault(parent_id, [])
                        if len(bucket) < _MAX_BUILD_LOG_LINES:
                            bucket.append(str(fields[0]))
        elif action == "msg":
            level = ev.get("level", 99)
            text = ev.get("msg") or ev.get("text") or ""
            # level 0 = error ONLY. Level 1 is warnings (e.g. "Git tree is
            # dirty"), which used to inflate the errors= summary counter.
            if level == 0 and text:
                with self._lock:
                    self.errors += 1
                    self.error_msgs.append(text)
        return False

    @staticmethod
    def _derivation_name(drv_path):
        """Extract `hello-2.12.1` from `/nix/store/<hash>-hello-2.12.1.drv`."""
        if not drv_path:
            return ""
        base = drv_path.rsplit("/", 1)[-1]
        if base.endswith(".drv"):
            base = base[:-4]
        if "-" in base:
            head, _, tail = base.partition("-")
            if len(head) == 32 and all(c in _NIX_HASH_CHARS for c in head):
                return tail
        return base

    def snapshot(self):
        with self._lock:
            current = (
                next(iter(self.running_builds.values()), None)
                or self.last_finished_name
                or ""
            )
            return {
                "built": self.started_builds,
                "finished": self.finished_builds,
                "fetched": self.started_fetches,
                "fetched_done": self.finished_fetches,
                "errors": self.errors,
                "current": current,
                "duration_s": round(time.monotonic() - self.start_t, 1),
            }

    def summary(self):
        snap = self.snapshot()
        return {
            "built": snap["built"],
            "fetched": snap["fetched"],
            "errors": snap["errors"],
            "duration_s": snap["duration_s"],
        }

    def failure_log(self):
        """Compact failure dump: error msgs + the largest captured build log."""
        with self._lock:
            parts = []
            if self.error_msgs:
                parts.extend(self.error_msgs[-50:])
            if self.build_logs:
                fid, lines = max(self.build_logs.items(), key=lambda kv: len(kv[1]))
                if lines:
                    parts.append("--- build log (id=%s) ---" % fid)
                    parts.extend(lines[-_MAX_FAILURE_LOG_LINES:])
            return "\n".join(parts)


def run_build(
    argv,
    *,
    cwd=None,
    env=None,
    display=None,
    output_mode="auto",
    nom_bin="nom",
    label="mandala.fleet.build",
    events=None,
):
    """Run a `nix build` invocation with dual-mode output.

    Returns: {rc, summary, build_log, out_paths, mode}.
    """
    mode = resolve_output_mode(output_mode)
    full_env = {**os.environ, **(env or {})}
    tracker = _Tracker()
    out_paths = []

    if display is not None:
        display.display("[%s] %s" % (label, quote_argv(argv)))
        display.display("[%s] output_mode=%s" % (label, mode))

    if mode == "tty":
        rc = _run_tty(argv, cwd, full_env, tracker, out_paths, nom_bin, display, label, events)
        if rc is None:
            # tty path bailed (no /dev/tty or no nom); fall through to flat.
            mode = "flat"
            rc = _run_flat(argv, cwd, full_env, tracker, out_paths, display, label, events)
    else:
        rc = _run_flat(argv, cwd, full_env, tracker, out_paths, display, label, events)

    summary = tracker.summary()
    if events is not None:
        events.progress(tracker.snapshot(), force=True)
    if display is not None:
        display.display(
            "[%s] done rc=%d built=%d fetched=%d errors=%d duration=%ss"
            % (label, rc, summary["built"], summary["fetched"], summary["errors"], summary["duration_s"])
        )

    return {
        "rc": rc,
        "summary": summary,
        "build_log": tracker.failure_log() if rc != 0 else "",
        "out_paths": out_paths,
        "mode": mode,
    }


def _run_tty(argv, cwd, env, tracker, out_paths, nom_bin, display, label, events=None):
    """nom owns /dev/tty; tee builder.stderr to nom AND tracker.

    Returns rc, or None if /dev/tty or nom is unavailable (caller falls back).
    """
    try:
        tty = open("/dev/tty", "wb", buffering=0)
    except OSError as e:
        if display is not None:
            display.warning("[%s] /dev/tty unavailable (%s); falling back to flat" % (label, e))
        return None

    nom = None
    builder = None
    try:
        try:
            nom = subprocess.Popen(
                [nom_bin, "--json"],
                stdin=subprocess.PIPE,
                stdout=tty.fileno(),
                stderr=tty.fileno(),
                bufsize=0,
                env=env,
            )
        except FileNotFoundError:
            if display is not None:
                display.warning("[%s] %s not on PATH; falling back to flat" % (label, nom_bin))
            return None

        builder = subprocess.Popen(
            argv,
            cwd=cwd,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0,
        )

        stdout_thread = threading.Thread(
            target=_drain_lines, args=(builder.stdout, out_paths), daemon=False,
        )
        tee_thread = threading.Thread(
            target=_tee_to_nom_and_tracker,
            args=(builder.stderr, nom.stdin, tracker, events),
            daemon=False,
        )
        stdout_thread.start()
        tee_thread.start()

        rc = builder.wait()
        tee_thread.join()
        try:
            nom.stdin.close()
        except Exception:
            pass
        nom_rc = nom.wait()
        stdout_thread.join()

        if nom_rc not in (0, None) and rc == 0 and display is not None:
            display.warning("[%s] nom exited rc=%d (build succeeded)" % (label, nom_rc))
        return rc
    finally:
        try:
            tty.close()
        except Exception:
            pass


def _run_flat(argv, cwd, env, tracker, out_paths, display, label, events=None):
    """No nom. Parse internal-json on stderr; emit periodic summary lines."""
    builder = subprocess.Popen(
        argv,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        bufsize=0,
    )

    last_emit = [0.0]
    last_sig = [None]

    def maybe_emit(force=False):
        now = time.monotonic()
        if not force and (now - last_emit[0]) < _SUMMARY_INTERVAL_S:
            return
        snap = tracker.snapshot()
        sig = (snap["built"], snap["finished"], snap["fetched"], snap["fetched_done"], snap["current"])
        if sig == last_sig[0] and not force:
            return
        last_sig[0] = sig
        last_emit[0] = now
        if display is not None:
            line = "[%s] built %d/%d, fetched %d/%d" % (
                label, snap["finished"], snap["built"],
                snap["fetched_done"], snap["fetched"],
            )
            if snap["current"]:
                line += ", current: %s" % snap["current"]
            display.display(line)

    def parse_stderr():
        for raw in iter(builder.stderr.readline, b""):
            line = raw.decode("utf-8", errors="replace").rstrip("\n")
            changed = tracker.feed_line(line)
            if line and not line.startswith(_NIX_JSON_PREFIX):
                if display is not None:
                    display.display("[%s] %s" % (label, line))
                if events is not None:
                    events.line(line, "nix")
            if changed:
                maybe_emit()
                if events is not None:
                    events.progress(tracker.snapshot())
        builder.stderr.close()

    stdout_thread = threading.Thread(
        target=_drain_lines, args=(builder.stdout, out_paths), daemon=False,
    )
    parse_thread = threading.Thread(target=parse_stderr, daemon=False)
    stdout_thread.start()
    parse_thread.start()

    rc = builder.wait()
    stdout_thread.join()
    parse_thread.join()
    maybe_emit(force=True)
    return rc


def _drain_lines(stream, sink):
    for raw in iter(stream.readline, b""):
        line = raw.decode("utf-8", errors="replace").rstrip("\n")
        if line:
            sink.append(line)
    stream.close()


def _tee_to_nom_and_tracker(builder_stderr, nom_stdin, tracker, events=None):
    """Forward bytes to nom; decode and feed tracker. Survives nom dying."""
    nom_alive = True
    for raw in iter(builder_stderr.readline, b""):
        if nom_alive:
            try:
                nom_stdin.write(raw)
                nom_stdin.flush()
            except (BrokenPipeError, ValueError, OSError):
                nom_alive = False
        try:
            changed = tracker.feed_line(raw.decode("utf-8", errors="replace").rstrip("\n"))
            if changed and events is not None:
                events.progress(tracker.snapshot())
        except Exception:
            pass
    builder_stderr.close()


def run_command_streaming(
    argv,
    *,
    cwd=None,
    env=None,
    display=None,
    label="cmd",
    output_mode="auto",
    events=None,
):
    """Run argv with live output. Used for deploy-rs (text status, not internal-json).

    Two paths, mirroring `run_build`:

      tty:  hand the child /dev/tty directly. Output goes straight to the
            user's terminal (truly live; SSH prompts / sudo password work).
            Display gets only start/end milestones.

      flat: allocate a PTY so the child's libc/Rust stdio sees a terminal
            and switches to line-buffered. Read the master fd, forward
            each line through display.display() in real time. Without a
            PTY, Rust full-buffers stdout on a pipe and the user sees
            nothing until the process exits.

    With `events` set, the PTY path is used unconditionally — handing the
    child /dev/tty would make its lines unobservable to the channel.

    Returns rc.
    """
    full_env = {**os.environ, **(env or {})}
    if display is not None:
        display.display("[%s] %s" % (label, quote_argv(argv)))

    mode = resolve_output_mode(output_mode) if events is None else "flat"
    if mode == "tty":
        rc = _run_command_tty(argv, cwd, full_env, display, label)
        if rc is None:
            mode = "flat"
            rc = _run_command_pty(argv, cwd, full_env, display, label, events)
    else:
        rc = _run_command_pty(argv, cwd, full_env, display, label, events)

    if display is not None:
        display.display("[%s] done rc=%d mode=%s" % (label, rc, mode))
    return rc


def _run_command_tty(argv, cwd, env, display, label):
    """Hand /dev/tty directly to the child for truly-live output.

    Returns rc, or None if /dev/tty is unavailable (caller falls back).
    """
    try:
        tty = open("/dev/tty", "r+b", buffering=0)
    except OSError as e:
        if display is not None:
            display.warning("[%s] /dev/tty unavailable (%s); falling back to PTY" % (label, e))
        return None
    try:
        proc = subprocess.Popen(
            argv,
            cwd=cwd,
            env=env,
            stdin=tty.fileno(),
            stdout=tty.fileno(),
            stderr=tty.fileno(),
        )
        return proc.wait()
    finally:
        try:
            tty.close()
        except Exception:
            pass


def _run_command_pty(argv, cwd, env, display, label, events=None):
    """Allocate a PTY so the child line-buffers; forward lines via display."""
    import pty as _pty

    master_fd, slave_fd = _pty.openpty()
    proc = None

    def handle(text):
        if display is not None:
            display.display("[%s] %s" % (label, text))
        if events is not None:
            events.feed_deploy_line(text)

    try:
        proc = subprocess.Popen(
            argv,
            cwd=cwd,
            env=env,
            stdin=slave_fd,
            stdout=slave_fd,
            stderr=slave_fd,
            close_fds=True,
        )
        os.close(slave_fd)
        slave_fd = -1

        buf = b""
        while True:
            try:
                chunk = os.read(master_fd, 4096)
            except OSError:
                break
            if not chunk:
                break
            # Progress bars (deploy-rs's inner nix, copy meters) redraw
            # with bare \r — treat every \r as a line break so each frame
            # becomes its own line instead of gluing into one mega-line.
            buf += chunk.replace(b"\r\n", b"\n").replace(b"\r", b"\n")
            while b"\n" in buf:
                line, _, buf = buf.partition(b"\n")
                text = line.decode("utf-8", errors="replace")
                if text:
                    handle(text)
        if buf:
            text = buf.decode("utf-8", errors="replace")
            if text:
                handle(text)
        return proc.wait()
    finally:
        if slave_fd >= 0:
            try:
                os.close(slave_fd)
            except Exception:
                pass
        try:
            os.close(master_fd)
        except Exception:
            pass
