# -*- coding: utf-8 -*-
"""Opt-in JSONL event channel — protocol v1.

The plugin↔porcelain contract: when MANDALA_FLEET_EVENTS names a writable
directory, the mandala.fleet action plugins append one JSON object per
line to <dir>/<inventory_hostname>.jsonl. Porcelain (a TUI, a log
collector) tails those files to render per-host state without parsing
ansible's display output. With the variable unset, NO code path changes:
emitters are never constructed and headless output is byte-identical.

Every event carries:
    v       protocol version (this file's PROTOCOL_VERSION; bump on any
            shape change — the protocol is the de-facto plugin API)
    ts      epoch seconds (float)
    host    inventory hostname the task ran for ("controller" if unknown)
    plugin  "build" | "deploy"
    event   one of the types below

Event types:
    status     {"state": "start", "cmd": [...]} / {"state": "done", "rc": N}
    line       {"line": "<raw output line>", "stream": "nix" | "deploy"}
    progress   build counters {"built", "finished", "fetched",
               "fetched_done", "errors", "current"} (rate-limited)
    milestone  {"milestone": "eval|build|copy|activate|wait|confirm|rollback"}
               parsed from deploy-rs status lines
    nixlog     {"line": "<raw '@nix {...}' internal-json line>"} — the
               verbatim nix log stream (v2). Porcelain can feed these
               straight into `nom --json` for a real build tree.

Protocol history: v2 adds the `nixlog` event type (additive — consumers
that ignore unknown event types read v2 streams as v1).

Emission is best-effort: I/O errors disable the emitter silently — the
event channel must never fail a deploy.
"""

from __future__ import absolute_import, division, print_function

__metaclass__ = type

import json
import os
import re
import time

PROTOCOL_VERSION = 2
ENV_VAR = "MANDALA_FLEET_EVENTS"

_PROGRESS_INTERVAL_S = 0.5

# deploy-rs status-line milestones, matched in order (first hit wins).
# Patterns track deploy-rs's human output; they are best-effort hints for
# porcelain, not load-bearing control flow.
_MILESTONES = (
    ("eval", re.compile(r"Evaluating flake")),
    ("build", re.compile(r"Building profile")),
    ("copy", re.compile(r"Copying profile")),
    ("activate", re.compile(r"Activating profile|activate the configuration")),
    ("wait", re.compile(r"Waiting for confirmation")),
    ("confirm", re.compile(r"Success activating|Completed dry-activate|[Dd]eployment confirmed")),
    ("rollback", re.compile(r"[Rr]olling back|[Mm]agic rollback")),
)


class Emitter:
    """Appends protocol-v1 events to <dir>/<host>.jsonl, best-effort."""

    def __init__(self, directory, host, plugin):
        self._host = host or "controller"
        self._plugin = plugin
        self._path = os.path.join(directory, "%s.jsonl" % self._host)
        self._fh = None
        self._dead = False
        self._last_progress = 0.0
        self._last_milestone = None

    @classmethod
    def from_env(cls, host, plugin, environ=None):
        """Emitter when the channel is opted into, else None (no-op path)."""
        directory = (environ or os.environ).get(ENV_VAR)
        if not directory:
            return None
        return cls(directory, host, plugin)

    def _emit(self, event, fields):
        if self._dead:
            return
        record = {
            "v": PROTOCOL_VERSION,
            "ts": time.time(),
            "host": self._host,
            "plugin": self._plugin,
            "event": event,
        }
        record.update(fields)
        try:
            if self._fh is None:
                os.makedirs(os.path.dirname(self._path), exist_ok=True)
                self._fh = open(self._path, "a", encoding="utf-8")
            self._fh.write(json.dumps(record, separators=(",", ":")) + "\n")
            self._fh.flush()
        except OSError:
            self._dead = True

    def status(self, state, **fields):
        fields["state"] = state
        self._emit("status", fields)

    def line(self, line, stream):
        self._emit("line", {"line": line, "stream": stream})

    def progress(self, snapshot, force=False):
        now = time.monotonic()
        if not force and (now - self._last_progress) < _PROGRESS_INTERVAL_S:
            return
        self._last_progress = now
        self._emit("progress", {
            k: snapshot[k]
            for k in ("built", "finished", "fetched", "fetched_done", "errors", "current")
            if k in snapshot
        })

    def milestone(self, name):
        if name == self._last_milestone:
            return
        self._last_milestone = name
        self._emit("milestone", {"milestone": name})

    def nixlog(self, line):
        """Verbatim '@nix {...}' internal-json line (v2) — nom food."""
        self._emit("nixlog", {"line": line})

    def feed_deploy_line(self, line):
        """line event + milestone detection for one deploy-rs output line."""
        self.line(line, "deploy")
        for name, pattern in _MILESTONES:
            if pattern.search(line):
                self.milestone(name)
                return

    def close(self):
        if self._fh is not None:
            try:
                self._fh.close()
            except OSError:
                pass
            self._fh = None
