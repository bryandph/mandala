"""Deployed-generation drift: contract vs reported fleet state.

Data path mirrors the survey pattern: the read-only state playbook
(mandala.fleet.state) fans out, reads each member's /run/current-system
and /run/booted-system links, and writes one JSON snapshot per host on
the controller. This module compares those snapshots against the
locally evaluated toplevels (`nixosConfigurations.<h>.config.system
.build.toplevel.outPath` — what /run/current-system points at after a
successful deploy).

Everything here is read-only: snapshots are files, expectations are a
nix eval, refresh is a fact-gather playbook. Nothing mutates a host.

Drift is EXACT out-path equality — deliberately strict: a commit that
only moves configurationRevision reads as drift, because the contract
the host should converge to has in fact moved. "In sync modulo
revision" would hide real drift behind label equality.
"""

from __future__ import annotations

import json
import os
import subprocess
from dataclasses import dataclass
from enum import Enum
from pathlib import Path

DEFAULT_STATE_DIR = Path(
    os.environ.get("MANDALA_FLEET_STATE", "/tmp/mandala-fleet-state")
)


class DriftStatus(str, Enum):
    IN_SYNC = "in-sync"
    DRIFT = "drift"
    REBOOT_PENDING = "reboot-pending"
    NO_SNAPSHOT = "no-snapshot"
    UNREACHABLE = "unreachable"


@dataclass
class DriftEntry:
    host: str
    status: DriftStatus
    expected: str | None = None
    current: str | None = None
    booted: str | None = None
    captured_at: str | None = None


def read_snapshots(state_dir: Path = DEFAULT_STATE_DIR) -> dict[str, dict]:
    """Per-host state JSON written by the state playbook."""
    snapshots: dict[str, dict] = {}
    if not Path(state_dir).is_dir():
        return snapshots
    for path in sorted(Path(state_dir).glob("*.json")):
        try:
            data = json.loads(path.read_text())
        except (OSError, ValueError):
            continue
        host = data.get("host") or path.stem
        snapshots[host] = data
    return snapshots


def eval_expected(flake: str, hosts: list[str]) -> dict[str, str]:
    """Locally evaluated toplevel out-paths for the given members.

    One nix eval for the whole set — heavy (it instantiates every
    requested system), so callers trigger it explicitly and cache.
    """
    names = json.dumps(hosts)
    expr = (
        "cfgs: builtins.listToAttrs (map (n: { name = n; "
        "value = cfgs.${n}.config.system.build.toplevel.outPath; }) "
        f"(builtins.fromJSON ''{names}''))"
    )
    out = subprocess.run(
        [
            "nix", "eval", "--no-warn-dirty", "--json",
            f"{flake}#nixosConfigurations", "--apply", expr,
        ],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(out)


def compare(
    deploy_nodes: list[str],
    snapshots: dict[str, dict],
    expected: dict[str, str] | None,
) -> list[DriftEntry]:
    """Drift table over the deploy-rs members."""
    entries = []
    for host in sorted(deploy_nodes):
        snap = snapshots.get(host)
        if snap is None:
            entries.append(DriftEntry(host=host, status=DriftStatus.NO_SNAPSHOT))
            continue
        if snap.get("unreachable"):
            entries.append(
                DriftEntry(host=host, status=DriftStatus.UNREACHABLE,
                           captured_at=snap.get("captured_at"))
            )
            continue
        current, booted = snap.get("current"), snap.get("booted")
        want = (expected or {}).get(host)
        if want is None or current is None:
            # No expectation evaluated (or snapshot incomplete): the only
            # judgement possible is current-vs-booted.
            status = (
                DriftStatus.REBOOT_PENDING
                if current and booted and current != booted
                else DriftStatus.NO_SNAPSHOT
                if current is None
                else DriftStatus.IN_SYNC
            )
        elif current != want:
            status = DriftStatus.DRIFT
        elif booted and booted != current:
            status = DriftStatus.REBOOT_PENDING
        else:
            status = DriftStatus.IN_SYNC
        entries.append(
            DriftEntry(
                host=host,
                status=status,
                expected=want,
                current=current,
                booted=booted,
                captured_at=snap.get("captured_at"),
            )
        )
    return entries


def refresh_snapshots(
    ansible_dir: Path,
    state_dir: Path = DEFAULT_STATE_DIR,
    limit: str | None = None,
) -> int:
    """Run the read-only state playbook (fact-gather; mutates nothing)."""
    env = dict(os.environ)
    env["MANDALA_FLEET_STATE"] = str(state_dir)
    argv = ["ansible-playbook", "mandala.fleet.state"]
    if limit:
        argv += ["-l", limit]
    return subprocess.run(argv, cwd=ansible_dir, env=env, check=False).returncode
