# -*- coding: utf-8 -*-
"""Action plugin for mandala.fleet.build.

Runs `nix build <installable> --log-format internal-json [...]` on the
ansible controller in one of two modes:

  - tty:  pipes through `nom` which owns /dev/tty (live tree)
  - flat: parses internal-json ourselves, emits periodic summary lines

Mode selection: `output_mode: auto|tty|flat` (default `auto`). `auto`
detects /dev/tty availability at runtime. The original stdout (carrying
e.g. `--print-out-paths` results) is captured and returned as `out_paths`.

When MANDALA_FLEET_EVENTS names a directory, progress/line events are
appended to <dir>/<host>.jsonl (module_utils.events, protocol v1);
without it, behavior is unchanged.
"""

from __future__ import absolute_import, division, print_function

__metaclass__ = type

from ansible.plugins.action import ActionBase
from ansible.utils.display import Display

from ansible_collections.mandala.fleet.plugins.module_utils.events import Emitter
from ansible_collections.mandala.fleet.plugins.module_utils.streamer import (
    quote_argv,
    run_build,
)

display = Display()


class ActionModule(ActionBase):
    TRANSFERS_FILES = False
    _requires_connection = False

    _VALID_ARGS = frozenset([
        "installable",
        "installables",
        "out_link",
        "print_out_paths",
        "impure",
        "cwd",
        "extra_args",
        "output_mode",
        "nom_bin",
    ])

    def run(self, tmp=None, task_vars=None):
        result = super(ActionModule, self).run(tmp, task_vars)
        del tmp

        args = self._task.args or {}
        installable = args.get("installable")
        installables = [str(i) for i in (args.get("installables") or [])]
        if installable and installables:
            result["failed"] = True
            result["msg"] = "mandala.fleet.build: 'installable' and 'installables' are mutually exclusive"
            return result
        if installable:
            installables = [str(installable)]
        if not installables:
            result["failed"] = True
            result["msg"] = "mandala.fleet.build: one of 'installable' or 'installables' is required"
            return result

        out_link = args.get("out_link")
        print_out_paths = bool(args.get("print_out_paths", True))
        impure = bool(args.get("impure", False))
        cwd = args.get("cwd")
        extra_args = list(args.get("extra_args") or [])
        output_mode = args.get("output_mode") or "auto"
        nom_bin = args.get("nom_bin") or "nom"

        argv = ["nix", "build"] + installables + ["--log-format", "internal-json"]
        if impure:
            argv.append("--impure")
        if out_link:
            argv += ["--out-link", str(out_link)]
        else:
            argv.append("--no-link")
        if print_out_paths:
            argv.append("--print-out-paths")
        argv += extra_args

        if self._play_context.check_mode:
            result["changed"] = False
            result["skipped"] = True
            result["msg"] = "check_mode: would have run %s" % quote_argv(argv)
            result["cmd"] = argv
            return result

        events = Emitter.from_env(
            host=(task_vars or {}).get("inventory_hostname"),
            plugin="build",
        )
        if events is not None:
            events.status("start", cmd=argv)

        out = run_build(
            argv,
            cwd=cwd,
            display=display,
            output_mode=output_mode,
            nom_bin=nom_bin,
            label="mandala.fleet.build",
            events=events,
        )

        rc = out["rc"]
        if events is not None:
            events.status("done", rc=rc)
            events.close()
        out_paths = [p for p in out["out_paths"] if p.startswith("/nix/store/")]

        result["rc"] = rc
        result["cmd"] = argv
        result["out_paths"] = out_paths
        result["summary"] = out["summary"]
        result["mode"] = out["mode"]
        result["changed"] = rc == 0 and out["summary"]["built"] > 0
        if rc != 0:
            result["failed"] = True
            result["msg"] = "nix build failed (rc=%d)" % rc
            if out["build_log"]:
                result["build_log"] = out["build_log"]
        return result
