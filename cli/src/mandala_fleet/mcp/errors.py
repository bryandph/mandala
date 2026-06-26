"""Structured subordinate-tool failure surfacing.

When the server dispatches a subordinate tool (nix eval/build, ansible,
deploy-rs) and it fails, the tool returns the captured output + exit
status as data the client can debug against — the same diagnostic the
operator reads in the failed host's raw stream — rather than an opaque
transport error. Shared by host-eval, drift, and the action tiers.
"""

from __future__ import annotations

import subprocess
from typing import Any


def failure(
    summary: str,
    *,
    command: list[str] | None = None,
    exit_code: int | None = None,
    output: str = "",
) -> dict[str, Any]:
    """A structured tool failure: `ok=False` plus the diagnostic context."""
    return {
        "ok": False,
        "error": summary,
        "command": command,
        "exit_code": exit_code,
        "output": output,
    }


def _cmd_list(args: Any) -> list[str] | None:
    if isinstance(args, (list, tuple)):
        return [str(a) for a in args]
    return [str(args)] if args else None


def from_called_process(
    summary: str, exc: subprocess.CalledProcessError
) -> dict[str, Any]:
    """Shape a CalledProcessError (with captured stderr/stdout) into a
    failure dict, preserving the command and exit code."""
    captured = ((exc.stderr or "") + (exc.stdout or "")).strip()
    return failure(
        summary, command=_cmd_list(exc.cmd), exit_code=exc.returncode, output=captured
    )


def from_completed(
    summary: str, proc: subprocess.CompletedProcess
) -> dict[str, Any]:
    """Shape a finished `subprocess.run` (captured output) into a failure
    dict — the action tiers capture rather than `check=True`, so a failed
    build/reboot returns the nix/ansible output instead of raising."""
    captured = ((proc.stderr or "") + (proc.stdout or "")).strip()
    return failure(
        summary, command=_cmd_list(proc.args), exit_code=proc.returncode, output=captured
    )
