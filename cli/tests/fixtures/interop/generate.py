#!/usr/bin/env python3
"""Regenerate the interop golden fixtures (fleet-state-formats spec).

This script IS the fixtures' provenance: the checked-in `state/` tree is
exactly what the PYTHON implementation writes through its real code paths
— `registry.write_meta`/`update_meta` (meta.json, atomic 1-space sorted
JSON), the `mandala.fleet` collection's `events.Emitter` (per-host event
JSONL, v1/v2), and `drift.save_expected` (`.expected.json`) — over fixed,
deterministic inputs (timestamps, pids, run ids). The Rust implementation
must read this tree identically (`crates/mandala-core/src/interop_tests.rs`),
and `cli/tests/test_interop_fixtures.py` asserts the Python reading of the
same bytes, so both implementations are pinned to ONE judgement over one
tree. Regenerate = run this script; never hand-edit the tree.

The only writer NOT exercised here is the snapshot writer — snapshots are
produced by the `mandala.fleet.state` playbook (ansible `copy` +
`to_nice_json`), so this script mirrors that playbook's field shape
(see ansible/collections/.../playbooks/state.yml) with plain json.dumps
(snapshots carry no byte-format contract; both readers just parse JSON).

Determinism knobs (so regeneration doesn't churn the tree):
- `time.time` is patched to a fixed 0.25s-step counter (Emitter `ts`).
- Run ids, pids, `started_at`/`finished_at` values are fixed constants.
- `events.PROTOCOL_VERSION` is patched per stream to produce genuine v1
  streams, v2 (nixlog) streams, and one unsupported-version (v99) record
  that every reader must SKIP while still consuming later records.

Run (from `flakes/mandala/`, any python with the repo on the path):

    PYTHONPATH=cli/src python3 cli/tests/fixtures/interop/generate.py
"""

from __future__ import annotations

import importlib.util
import json
import shutil
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(REPO / "cli/src"))

from mandala_fleet import drift, registry  # noqa: E402

# The collection's event writer — the real JSONL producer the ansible
# plugins use. Imported straight from its file: no ansible needed.
_EVENTS_PY = (
    REPO
    / "ansible/collections/ansible_collections/mandala/fleet"
    / "plugins/module_utils/events.py"
)
_spec = importlib.util.spec_from_file_location("mandala_fleet_events", _EVENTS_PY)
events = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(events)

STATE = Path(__file__).resolve().parent / "state"

# ---- deterministic inputs ------------------------------------------------

BASE_TS = 1767225600.0  # 2026-01-01T00:00:00Z, exact binary fraction steps


class _Clock:
    """A fixed-step stand-in for time.time (0.25s steps repr cleanly)."""

    def __init__(self) -> None:
        self.now = BASE_TS

    def __call__(self) -> float:
        self.now += 0.25
        return self.now


REV = "0123456789abcdef0123456789abcdef01234567"

# host -> expected toplevel (the .expected.json cache). epsilon/zeta/eta
# deliberately absent — a cache never covers the whole fleet.
TOPLEVELS = {
    "alpha": "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-nixos-system-alpha-26.05",
    "beta": "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-nixos-system-beta-26.05",
    "gamma": "/nix/store/cccccccccccccccccccccccccccccccc-nixos-system-gamma-26.05",
    "delta": "/nix/store/dddddddddddddddddddddddddddddddd-nixos-system-delta-26.05",
    "theta": "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-nixos-system-theta-26.05",
}

FRESH = "2026-01-01T12:00:00+00:00"  # 12h before the tests' fixed "now"
OLD = "2025-12-01T00:00:00+00:00"  # long past DEFAULT_MAX_AGE

_QUAD = {
    "kernel": "/nix/store/kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk-linux-6.12.30",
    "kernel_modules": "/nix/store/mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm-kernel-modules",
    "initrd": "/nix/store/iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii-initrd-linux-6.12.30",
    "kernel_params": "console=tty0 loglevel=4",
}
_QUAD_NEW_KERNEL = {
    **_QUAD,
    "kernel": "/nix/store/nnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn-linux-6.12.31",
}
# Same params as a TOKEN SEQUENCE — the survey's echo wrapper may add
# whitespace; both comparators must normalize (activated, not pending).
_QUAD_WS_PARAMS = {**_QUAD, "kernel_params": " console=tty0  loglevel=4 "}


def _snapshot(host: str, **fields) -> None:
    """Mirror the state playbook's per-host snapshot shape (state.yml)."""
    snap = {
        "host": host,
        "unreachable": False,
        "current": None,
        "booted": None,
        "current_boot": None,
        "booted_boot": None,
        "captured_at": FRESH,
        **fields,
    }
    (STATE / f"{host}.json").write_text(json.dumps(snap, indent=4) + "\n")


def write_snapshots() -> None:
    # in-sync: current == booted == expected.
    _snapshot("alpha", current=TOPLEVELS["alpha"], booted=TOPLEVELS["alpha"],
              current_boot=_QUAD, booted_boot=_QUAD)
    # drift: current != expected.
    _snapshot("beta",
              current="/nix/store/ffffffffffffffffffffffffffffffff-nixos-system-beta-26.04",
              booted="/nix/store/ffffffffffffffffffffffffffffffff-nixos-system-beta-26.04",
              current_boot=_QUAD, booted_boot=_QUAD)
    # reboot-pending: booted != current AND the kernel moved.
    _snapshot("gamma", current=TOPLEVELS["gamma"],
              booted="/nix/store/99999999999999999999999999999999-nixos-system-gamma-26.04",
              current_boot=_QUAD_NEW_KERNEL, booted_boot=_QUAD)
    # activated: booted != current, quad identical modulo params whitespace.
    _snapshot("delta", current=TOPLEVELS["delta"],
              booted="/nix/store/88888888888888888888888888888888-nixos-system-delta-26.04",
              current_boot=_QUAD, booted_boot=_QUAD_WS_PARAMS)
    # unreachable: the survey could not reach the host.
    _snapshot("epsilon", unreachable=True)
    # incomplete: reached, but no system links landed.
    _snapshot("zeta")
    # stale: a fresh-looking snapshot with an old capture time.
    _snapshot("theta", current=TOPLEVELS["theta"], booted=TOPLEVELS["theta"],
              current_boot=_QUAD, booted_boot=_QUAD, captured_at=OLD)
    # eta: NO snapshot file (no-snapshot status).


# ---- registry runs -------------------------------------------------------

RUN_A = "20260101T000000_000000-1001"  # deploy, all hosts confirmed
RUN_B = "20260101T000100_000000-1002"  # deploy, rollback + sticky-confirmed
RUN_C = "20260101T000200_000000-1003"  # deploy, batch-build death (no hosts)
RUN_D = "20260101T000300_000000-1004"  # command (reboot), reaped rc=3
RUN_E = "20260101T000400_000000-1005"  # command (build), pid "still alive"

# Fake pids, unique per run so tests can fake liveness per pid. 111111 etc.
# are dead at test time; tests treat 555555 as the one live foreign pid.
PIDS = {RUN_A: 111111, RUN_B: 222222, RUN_C: 333333, RUN_D: 444444, RUN_E: 555555}

# The deploy-rs-shaped output lines feed_deploy_line milestones from —
# the REAL detection path (events.py _MILESTONES), not synthetic records.
_DEPLOY_LINES = (
    "Evaluating flake in .",
    "Building profile `system` for node `{h}`",
    "Copying profile `system` to node `{h}`",
    "Activating profile `system` on node `{h}`",
    "Waiting for confirmation, this may take a minute...",
    "Success activating profile `system` on node `{h}`",
)


def _confirm_chain(em, host: str) -> None:
    for line in _DEPLOY_LINES:
        em.feed_deploy_line(line.format(h=host))


def _deploy_meta(run_id: str, limit: str, throttle: int, playbook: str,
                 started_at: float) -> dict:
    """The exact field set DeployRun.start records (runner.py) — no argv."""
    return {
        "run_id": run_id,
        "limit": limit,
        "dry_activate": False,
        "throttle": throttle,
        "playbook": playbook,
        "pid": PIDS[run_id],
        "started_at": started_at,
    }


def write_runs() -> None:
    runs = STATE / "runs"

    # RUN_A — a finished deploy: alpha (v1) + beta (v2 with nixlog) both
    # confirm; the build stream (controller.jsonl) completes rc=0; beta's
    # stream ends in a TORN final line (the partial-write the tailers must
    # re-read, not consume) whose remainder lives in beta.jsonl.tail.
    a = runs / RUN_A
    a.mkdir(parents=True)
    registry.write_meta(a, _deploy_meta(RUN_A, "alpha,beta", 4,
                                        "mandala.fleet.deploy", BASE_TS))
    events.PROTOCOL_VERSION = 1
    em = events.Emitter(str(a), "alpha", "deploy")
    _confirm_chain(em, "alpha")
    em.status("done", rc=0)
    em.close()

    events.PROTOCOL_VERSION = 2
    em = events.Emitter(str(a), "beta", "deploy")
    _confirm_chain(em, "beta")
    em.nixlog('@nix {"action":"start","id":1,"type":105,"text":"building beta"}')
    em.status("done", rc=0)
    em.line("tail-marker-line", "deploy")  # the record to tear
    em.close()
    beta = a / "beta.jsonl"
    raw = beta.read_bytes()
    cut = raw.rstrip(b"\n").rfind(b"\n") + 1 + 25  # 25 bytes into the last record
    beta.write_bytes(raw[:cut])
    (a / "beta.jsonl.tail").write_bytes(raw[cut:])

    em = events.Emitter(str(a), None, "build")  # -> controller.jsonl
    em.status("start", cmd=["nix", "build", "--no-link"])
    em.progress({"built": 3, "finished": 2, "fetched": 5, "fetched_done": 5,
                 "errors": 0, "current": "nixos-system-alpha"}, force=True)
    em.line("these 2 derivations will be built:", "nix")
    em.status("done", rc=0)
    em.close()

    # RUN_B — rollback wins over confirmed (gamma), a late done rc=1 does
    # NOT unflag a confirmed host (delta), and an unsupported-version (v99)
    # record mid-stream is skipped while LATER records are still consumed.
    b = runs / RUN_B
    b.mkdir(parents=True)
    registry.write_meta(b, _deploy_meta(RUN_B, "gamma,delta", 2,
                                        "playbooks/deploy.yaml", BASE_TS + 60.0))
    events.PROTOCOL_VERSION = 1
    em = events.Emitter(str(b), "gamma", "deploy")
    _confirm_chain(em, "gamma")
    em.feed_deploy_line("Rolling back to previous generation")
    em.status("done", rc=1)
    em.close()

    em = events.Emitter(str(b), "delta", "deploy")
    _confirm_chain(em, "delta")
    events.PROTOCOL_VERSION = 99  # a future protocol both readers must skip
    em.line("future-protocol-noise", "deploy")
    events.PROTOCOL_VERSION = 1
    em.status("done", rc=1)  # consumed (records AFTER the v99 one still land)
    em.close()

    # RUN_C — batch-build death: the build stream failed before any host
    # event existed. Liveness must judge FAILED, not unknown.
    c = runs / RUN_C
    c.mkdir(parents=True)
    registry.write_meta(c, _deploy_meta(RUN_C, "epsilon", 4,
                                        "mandala.fleet.deploy", BASE_TS + 120.0))
    events.PROTOCOL_VERSION = 2
    em = events.Emitter(str(c), None, "build")
    em.status("start", cmd=["nix", "build", "--no-link"])
    em.line("error: builder for '/nix/store/zz-oops.drv' failed", "nix")
    em.status("done", rc=2)
    em.close()

    # RUN_D — a command run (reboot) whose launcher's reaper already
    # recorded the exit code: the reaped-rc liveness path, via the SAME
    # write_meta -> update_meta sequence CommandRun uses.
    d = runs / RUN_D
    d.mkdir(parents=True)
    argv = ["ansible-playbook", "playbooks/reboot.yaml", "-l", "alpha",
            "-e", "reboot_serial=1", "-e", "drain=true"]
    registry.write_meta(d, {
        "run_id": RUN_D,
        "kind": "reboot",
        "pid": PIDS[RUN_D],
        "argv": argv,
        "started_at": BASE_TS + 180.0,
    })
    registry.update_meta(d, rc=3, finished_at=BASE_TS + 190.0)
    (d / "output.log").write_text(
        f"$ {' '.join(argv)}  (cwd=ansible)\n"
        "PLAY [reboot] *****\n"
        "fatal: [alpha]: FAILED!\n"
    )

    # RUN_E — a command run (build) still in flight: pid recorded, no rc.
    # Tests fake its pid ALIVE — the foreign live run pruning must spare.
    e = runs / RUN_E
    e.mkdir(parents=True)
    argv = ["nix", "build", "--no-link", "--print-out-paths", ".#deployBatch.all"]
    registry.write_meta(e, {
        "run_id": RUN_E,
        "kind": "build",
        "pid": PIDS[RUN_E],
        "argv": argv,
        "started_at": BASE_TS + 240.0,
    })
    (e / "output.log").write_text(f"$ {' '.join(argv)}  (cwd=.)\n")


def main() -> None:
    import time

    if STATE.exists():
        shutil.rmtree(STATE)
    STATE.mkdir(parents=True)
    time.time = _Clock()  # deterministic Emitter `ts` values

    write_snapshots()
    drift.save_expected(REV, TOPLEVELS, STATE)
    write_runs()
    print(f"regenerated {STATE}")


if __name__ == "__main__":
    main()
