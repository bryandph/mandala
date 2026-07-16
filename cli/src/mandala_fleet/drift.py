"""Deployed-generation drift: contract vs reported fleet state.

Data path mirrors the survey pattern: the read-only state playbook
(mandala.fleet.state) fans out, reads each member's /run/current-system
and /run/booted-system links plus each system's boot-critical facts,
and writes one JSON snapshot per host on the controller. This module compares those snapshots against the
locally evaluated toplevels (`nixosConfigurations.<h>.config.system
.build.toplevel.outPath` — what /run/current-system points at after a
successful deploy).

Everything here is read-only: snapshots are files, expectations are a
nix eval, refresh is a fact-gather playbook. Nothing mutates a host.

Drift is EXACT out-path equality — deliberately strict: a moved
contract IS drift. Time gets the same strictness: snapshots older than
the staleness threshold judge as STALE rather than pretending an old
observation is current.

A booted/current split is judged by its boot-critical subset — kernel,
kernel-modules, initrd, kernel-params: what switch-to-configuration
cannot apply live (the same quad nixos-needsreboot compares). Only a
change there is REBOOT_PENDING; otherwise the new generation is fully
live and reports ACTIVATED.

State lives under $MANDALA_FLEET_STATE, else $XDG_STATE_HOME/mandala/
fleet (resolved at CALL time, not import time): per-user, persistent
across reboots, and not a predictable world-writable-parent /tmp path
another local user could pre-seed. Snapshots are keyed by FILENAME —
the survey writes <inventory_hostname>.json — never by a host field
inside the file, so one file cannot impersonate another host.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from enum import Enum
from pathlib import Path

# Evaluated expectations, cached keyed by the contract's git rev: equal
# CLEAN revs guarantee an identical contract, so the slow toplevel eval
# is reusable until the repo actually moves — and a key mismatch IS the
# "contract moved since last eval" signal.
_EXPECTED_CACHE = ".expected.json"

# Past this age a snapshot no longer supports any in-sync/drift claim.
DEFAULT_MAX_AGE = timedelta(hours=24)

_NAME_RE = re.compile(r"[A-Za-z0-9._-]+")


def state_dir() -> Path:
    """The snapshot/cache directory, resolved at call time."""
    env = os.environ.get("MANDALA_FLEET_STATE")
    if env:
        return Path(env)
    xdg = os.environ.get("XDG_STATE_HOME") or (Path.home() / ".local/state")
    return Path(xdg) / "mandala" / "fleet"


class DriftStatus(str, Enum):
    IN_SYNC = "in-sync"
    DRIFT = "drift"
    REBOOT_PENDING = "reboot-pending"  # boot-critical change awaits a reboot
    ACTIVATED = "activated"  # booted != current, but nothing boot-critical moved
    STALE = "stale"  # snapshot too old to support a judgement
    INCOMPLETE = "incomplete"  # snapshot exists but lacks the system links
    NO_SNAPSHOT = "no-snapshot"  # never surveyed
    UNREACHABLE = "unreachable"


# One styling vocabulary for every presentation surface (rich CLI table,
# textual drift tab) — keeping it beside the enum means a new status
# cannot ship without a style, which the UIs would otherwise KeyError on.
STATUS_STYLE: dict[DriftStatus, str] = {
    DriftStatus.IN_SYNC: "green",
    DriftStatus.DRIFT: "bold red",
    DriftStatus.REBOOT_PENDING: "yellow",
    DriftStatus.ACTIVATED: "dim green",
    DriftStatus.STALE: "dim yellow",
    DriftStatus.INCOMPLETE: "dim red",
    DriftStatus.NO_SNAPSHOT: "dim",
    DriftStatus.UNREACHABLE: "magenta",
}


@dataclass
class DriftEntry:
    host: str
    status: DriftStatus
    expected: str | None = None
    current: str | None = None
    booted: str | None = None
    captured_at: str | None = None


def read_snapshots(directory: Path | None = None) -> dict[str, dict]:
    """Per-host state JSON written by the state playbook, keyed by file
    stem (the inventory hostname the survey wrote it under)."""
    directory = state_dir() if directory is None else Path(directory)
    snapshots: dict[str, dict] = {}
    if not directory.is_dir():
        return snapshots
    for path in sorted(directory.glob("*.json")):
        try:
            snapshots[path.stem] = json.loads(path.read_text())
        except (OSError, ValueError):
            continue
    return snapshots


def eval_expected(flake: str, hosts: list[str]) -> dict[str, str]:
    """Locally evaluated toplevel out-paths for the given members.

    One nix eval for the whole set — heavy (it instantiates every
    requested system), so callers trigger it explicitly and cache via
    save_expected. Host names are validated before they enter the Nix
    expression: the aggregate is a versioned trust boundary, and a name
    containing `''` would otherwise escape the indented string.
    """
    for name in hosts:
        if not _NAME_RE.fullmatch(name):
            raise ValueError(f"refusing to eval: invalid member name {name!r}")
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


def repo_rev(flake: str) -> str | None:
    """The contract's git rev, '-dirty'-suffixed when the tree is. Cheap."""
    try:
        rev = subprocess.run(
            ["git", "-C", flake, "rev-parse", "HEAD"],
            check=True, capture_output=True, text=True,
        ).stdout.strip()
        dirty = subprocess.run(
            ["git", "-C", flake, "status", "--porcelain"],
            check=True, capture_output=True, text=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None
    return f"{rev}-dirty" if dirty else rev


def short_rev(rev: str | None) -> str:
    """Abbreviate a rev for display, keeping the '-dirty' suffix — losing
    it makes 'cache @ X, repo @ X' read as a contradiction."""
    if rev is None:
        return "?"
    if rev.endswith("-dirty"):
        return f"{rev.removesuffix('-dirty')[:11]}-dirty"
    return rev[:11]


def cache_fresh(cached_rev: str | None, current_rev: str | None) -> bool:
    """A cached expectation is reusable only for the SAME CLEAN rev —
    dirty trees have unknowable content and never match."""
    return (
        cached_rev is not None
        and current_rev is not None
        and cached_rev == current_rev
        and not current_rev.endswith("-dirty")
    )


def load_expected(directory: Path | None = None) -> tuple[str | None, dict[str, str]]:
    """(rev the cache was evaluated at, host -> toplevel out-path)."""
    directory = state_dir() if directory is None else Path(directory)
    try:
        data = json.loads((directory / _EXPECTED_CACHE).read_text())
    except (OSError, ValueError):
        return None, {}
    return data.get("rev"), data.get("toplevels", {})


def save_expected(
    rev: str | None,
    toplevels: dict[str, str],
    directory: Path | None = None,
) -> None:
    directory = state_dir() if directory is None else Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    (directory / _EXPECTED_CACHE).write_text(
        json.dumps({"rev": rev, "toplevels": toplevels}, indent=1, sort_keys=True)
    )


def _too_old(captured_at: str | None, max_age: timedelta | None, now: datetime) -> bool:
    if max_age is None or not captured_at:
        return False
    try:
        when = datetime.fromisoformat(captured_at)
    except ValueError:
        return True  # unparseable timestamp can't support a judgement
    if when.tzinfo is None:
        when = when.replace(tzinfo=timezone.utc)  # the playbook writes UTC
    return (now - when) > max_age


# Boot-critical subset of a toplevel (see module docstring): compared
# between booted and current to decide REBOOT_PENDING vs ACTIVATED.
_BOOT_CRITICAL = ("kernel", "kernel_modules", "initrd", "kernel_params")


def _boot_critical_changed(snap: dict) -> bool:
    """Whether booted -> current crosses a boot-critical change.

    Conservative: a snapshot without boot facts (written by a pre-upgrade
    survey) or with a fact missing on either side judges as changed — an
    unproven reboot-safety claim must not soften REBOOT_PENDING.
    """
    current, booted = snap.get("current_boot"), snap.get("booted_boot")
    if not isinstance(current, dict) or not isinstance(booted, dict):
        return True
    for key in _BOOT_CRITICAL:
        a, b = current.get(key), booted.get(key)
        if not a or not b:
            return True
        if key == "kernel_params":
            # The cmdline is compared as a token sequence — the survey's
            # echo wrapper may introduce surrounding whitespace.
            a, b = " ".join(a.split()), " ".join(b.split())
        if a != b:
            return True
    return False


def compare(
    deploy_nodes: list[str],
    snapshots: dict[str, dict],
    expected: dict[str, str] | None,
    max_age: timedelta | None = DEFAULT_MAX_AGE,
    now: datetime | None = None,
) -> list[DriftEntry]:
    """Drift table over the deploy-rs members."""
    now = now or datetime.now(timezone.utc)
    entries = []
    for host in sorted(deploy_nodes):
        snap = snapshots.get(host)
        if snap is None:
            entries.append(DriftEntry(host=host, status=DriftStatus.NO_SNAPSHOT))
            continue
        current, booted = snap.get("current"), snap.get("booted")
        captured_at = snap.get("captured_at")
        entry = DriftEntry(
            host=host,
            status=DriftStatus.IN_SYNC,
            expected=(expected or {}).get(host),
            current=current,
            booted=booted,
            captured_at=captured_at,
        )
        if snap.get("unreachable"):
            entry.status = DriftStatus.UNREACHABLE
        elif current is None:
            # The survey reached the host but got no system links —
            # distinct from "never surveyed" (a broken fact-gather).
            entry.status = DriftStatus.INCOMPLETE
        elif _too_old(captured_at, max_age, now):
            entry.status = DriftStatus.STALE
        elif entry.expected is not None and current != entry.expected:
            entry.status = DriftStatus.DRIFT
        elif booted and booted != current:
            entry.status = (
                DriftStatus.REBOOT_PENDING
                if _boot_critical_changed(snap)
                else DriftStatus.ACTIVATED
            )
        else:
            entry.status = DriftStatus.IN_SYNC
        entries.append(entry)
    return entries


def refresh_snapshots(
    ansible_dir: Path,
    directory: Path | None = None,
    limit: str | None = None,
) -> int:
    """Run the read-only state playbook (fact-gather; mutates nothing)."""
    env = dict(os.environ)
    env["MANDALA_FLEET_STATE"] = str(state_dir() if directory is None else directory)
    argv = ["ansible-playbook", "mandala.fleet.state"]
    if limit:
        argv += ["-l", limit]
    return subprocess.run(argv, cwd=ansible_dir, env=env, check=False).returncode
