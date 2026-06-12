# -*- coding: utf-8 -*-
"""Action plugin for mandala.fleet.deploy.

Two phases:

  1. Optional `nix build` of the deploy-rs system toplevel, with `nom` /
     internal-json rendering (same dual-mode as mandala.fleet.build).
     Auto-enabled when `target` is a single `.#<name>` reference; opt out
     with `prebuild: false`.

  2. `deploy` from deploy-rs against the (now cached) target. deploy-rs
     emits free-form text status, NOT internal-json — `nom` would do
     nothing useful with it — so its output is line-streamed raw through
     ansible's display.

When prebuild runs, `--skip-checks` is forced for the deploy phase: the
build is already cached, and deploy-rs's `nix flake check` would just
duplicate work (and often fails in flakes that need `--impure`).

When MANDALA_FLEET_EVENTS names a directory, raw lines + parsed
milestones (eval/build/copy/activate/wait/confirm/rollback) are appended
to <dir>/<host>.jsonl (module_utils.events, protocol v1); without it,
behavior is unchanged.
"""

from __future__ import absolute_import, division, print_function

__metaclass__ = type

import re

from ansible.plugins.action import ActionBase
from ansible.utils.display import Display

from ansible_collections.mandala.fleet.plugins.module_utils.events import Emitter
from ansible_collections.mandala.fleet.plugins.module_utils.streamer import (
    quote_argv,
    run_build,
    run_command_streaming,
)

display = Display()

# `.#vishnu`, `./flake#vishnu`, `github:foo/bar#vishnu` — capture host name.
_TARGET_NAME_RE = re.compile(r"#([A-Za-z0-9_.-]+)$")


class ActionModule(ActionBase):
    TRANSFERS_FILES = False
    _requires_connection = False

    _VALID_ARGS = frozenset([
        "target",
        "targets",
        "dry_activate",
        "skip_checks",
        "magic_rollback",
        "auto_rollback",
        "confirm_timeout",
        "hostname",
        "ssh_user",
        "remote_build",
        "keep_result",
        "log_dir",
        "cwd",
        "extra_args",
        "nix_extra_args",
        "prebuild",
        "prebuild_attr",
        "output_mode",
        "nom_bin",
    ])

    def run(self, tmp=None, task_vars=None):
        result = super(ActionModule, self).run(tmp, task_vars)
        del tmp

        args = self._task.args or {}
        target = args.get("target")
        targets = list(args.get("targets") or [])
        if target and targets:
            result["failed"] = True
            result["msg"] = "mandala.fleet.deploy: 'target' and 'targets' are mutually exclusive"
            return result
        if not target and not targets:
            result["failed"] = True
            result["msg"] = "mandala.fleet.deploy: one of 'target' or 'targets' is required"
            return result

        cwd = args.get("cwd")
        output_mode = args.get("output_mode") or "auto"
        nom_bin = args.get("nom_bin") or "nom"

        # Resolve prebuild: default ON when target is single `.#<name>`, OFF
        # for multi-targets / no name. `prebuild_attr` overrides auto-derivation.
        prebuild_attr = args.get("prebuild_attr")
        prebuild_default = bool(target and _TARGET_NAME_RE.search(target))
        prebuild = bool(args.get("prebuild", prebuild_default))
        if prebuild and not prebuild_attr and target:
            m = _TARGET_NAME_RE.search(target)
            if m:
                prefix = target[: m.start()] or "."
                prebuild_attr = "%s#deploy.nodes.%s.profiles.system.path" % (
                    prefix, m.group(1),
                )
        if prebuild and not prebuild_attr:
            prebuild = False

        skip_checks = bool(args.get("skip_checks", False))
        if prebuild:
            skip_checks = True

        if self._play_context.check_mode:
            result["changed"] = False
            result["skipped"] = True
            result["msg"] = "check_mode: would prebuild=%s, then deploy %s" % (
                prebuild_attr or "n/a", target or " ".join(targets),
            )
            return result

        events = Emitter.from_env(
            host=(task_vars or {}).get("inventory_hostname"),
            plugin="deploy",
        )

        if prebuild:
            display.display("[mandala.fleet.deploy] prebuild: %s" % prebuild_attr)
            prebuild_argv = [
                "nix", "build", prebuild_attr,
                "--log-format", "internal-json",
                "--no-link",
                "--print-out-paths",
            ]
            pre = run_build(
                prebuild_argv,
                cwd=cwd,
                display=display,
                output_mode=output_mode,
                nom_bin=nom_bin,
                label="mandala.fleet.deploy:build",
                events=events,
            )
            if pre["rc"] != 0:
                if events is not None:
                    events.status("done", rc=pre["rc"])
                    events.close()
                result["failed"] = True
                result["msg"] = "prebuild failed (rc=%d) for %s" % (pre["rc"], prebuild_attr)
                result["rc"] = pre["rc"]
                result["build_summary"] = pre["summary"]
                if pre["build_log"]:
                    result["build_log"] = pre["build_log"]
                return result
            result["build_summary"] = pre["summary"]
            result["prebuild_out_paths"] = [
                p for p in pre["out_paths"] if p.startswith("/nix/store/")
            ]

        # Deploy phase.
        dry_activate = bool(args.get("dry_activate", False))
        magic_rollback = args.get("magic_rollback")
        auto_rollback = args.get("auto_rollback")
        confirm_timeout = args.get("confirm_timeout")
        hostname = args.get("hostname")
        ssh_user = args.get("ssh_user")
        remote_build = bool(args.get("remote_build", False))
        keep_result = bool(args.get("keep_result", False))
        log_dir = args.get("log_dir")
        extra_args = list(args.get("extra_args") or [])
        nix_extra_args = list(args.get("nix_extra_args") or [])

        argv = ["deploy"]
        if skip_checks:
            argv.append("--skip-checks")
        if dry_activate:
            argv.append("--dry-activate")
        if magic_rollback is not None:
            argv += ["--magic-rollback", "true" if magic_rollback else "false"]
        if auto_rollback is not None:
            argv += ["--auto-rollback", "true" if auto_rollback else "false"]
        if confirm_timeout is not None:
            argv += ["--confirm-timeout", str(int(confirm_timeout))]
        if hostname:
            argv += ["--hostname", str(hostname)]
        if ssh_user:
            argv += ["--ssh-user", str(ssh_user)]
        if remote_build:
            argv.append("--remote-build")
        if keep_result:
            argv.append("--keep-result")
        if log_dir:
            argv += ["--log-dir", str(log_dir)]
        argv += extra_args
        if targets:
            argv += ["--targets"] + [str(t) for t in targets]
        elif target:
            argv.append(str(target))
        if nix_extra_args:
            argv += ["--"] + [str(a) for a in nix_extra_args]

        if events is not None:
            events.status("start", cmd=argv)

        rc = run_command_streaming(
            argv,
            cwd=cwd,
            display=display,
            label="mandala.fleet.deploy",
            output_mode=output_mode,
            events=events,
        )

        if events is not None:
            events.status("done", rc=rc)
            events.close()

        result["rc"] = rc
        result["cmd"] = argv
        result["changed"] = rc == 0 and not dry_activate
        if rc != 0:
            result["failed"] = True
            result["msg"] = "deploy failed (rc=%d)" % rc
        return result
