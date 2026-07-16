"""FastMCP server over the fleet cores — read + drift surface (the action
tiers are added in a later increment).

Pure presentation: every tool delegates to the inventory/drift cores, the
same ones the CLI and TUI read, so a selector here resolves to exactly what
`mandala resolve` and `ansible -l` project. The slow inputs (a host's
`toplevel` eval, the drift survey, the expected-toplevel eval) run only when
a tool argument explicitly asks for them, mirroring the CLI's opt-ins.
"""

from __future__ import annotations

import os
import re
import subprocess
import time
from collections import Counter
from dataclasses import asdict

from fastmcp import FastMCP
from fastmcp.exceptions import ToolError

from .. import drift as drift_mod
from ..inventory import Inventory, InventoryError, surfaces
from ..registry import RunLiveness
from ..runner import COMMAND_LOG, HostState, ansible_dir, reboot_argv
from .errors import from_called_process


def _adhoc_env() -> dict[str, str]:
    """Env for ad-hoc ansible runs: silence deprecation chatter that would
    otherwise ride along in every tool result."""
    return dict(os.environ, ANSIBLE_DEPRECATION_WARNINGS="False")


# systemd unit names an MCP client may restart: a plain name (dots, @, :)
# only — anything shell-ish or path-ish is refused before ansible sees it.
_UNIT_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9@:._-]*$")

# The reboot playbook's `serial`: a batch count or a percentage. Anything
# else is refused — ansible parses `-e "a=1 b=2"` as MULTIPLE extra-vars,
# so an unvalidated string here would be an extra-vars injection point.
_SERIAL_RE = re.compile(r"^[0-9]+%?$")

# Blocking waits stay under typical MCP client timeouts.
_MAX_WAIT_SECONDS = 570


def build_server(inventory, activity_sink=None, set_inventory=None) -> FastMCP:
    """A transport-agnostic FastMCP server over the inventory. Registering
    tools never evaluates the fleet — the first read triggers the one
    `nix eval .#mandala`, gated by schemaVersion in the inventory core.

    `inventory` is an Inventory OR a zero-arg callable returning the
    current one — the TUI passes a getter so its `r` reload (which rebinds
    a fresh Inventory) is what the hosted server serves, not the object
    captured at launch. `set_inventory`, when given, is how the `reload`
    tool commits a freshly evaluated Inventory back to the host; with a
    plain Inventory it defaults to an internal slot swap.

    `activity_sink`, when given, receives `{tool, args, status, detail,
    seq, elapsed, result}` per tool call (the TUI-hosted transport feeds
    this into its activity pane). Mutating calls are ALWAYS appended to
    the per-user audit log, sink or no sink."""
    if callable(inventory):
        get_inv = inventory
    else:
        _slot = [inventory]
        get_inv = lambda: _slot[0]  # noqa: E731
        if set_inventory is None:
            set_inventory = lambda new: _slot.__setitem__(0, new)  # noqa: E731

    mcp = FastMCP("mandala-fleet")

    from .activity import ActivityMiddleware, audit_event

    def _sink(event: dict) -> None:
        audit_event(event)
        if activity_sink is not None:
            activity_sink(event)

    mcp.add_middleware(ActivityMiddleware(_sink))

    @mcp.tool
    def members(full: bool = False) -> dict:
        """Every fleet member (NixOS + facts-only). Compact by default —
        platform, arch, category, role, tags, surfaces (a=ansible
        d=deploy-rs s=sops) per member — because the full aggregate dump
        is tens of KB and blows client tool-result caps. `full=true`
        returns everything; `host_eval` gives one member's full record."""
        try:
            roster = get_inv().members
        except InventoryError as e:
            raise ToolError(str(e)) from e
        if full:
            return roster
        keep = ("platform", "architecture", "category", "role", "tags")
        return {
            name: {
                **{k: m[k] for k in keep if k in m},
                "surfaces": surfaces(m),
            }
            for name, m in roster.items()
        }

    @mcp.tool
    def groups() -> dict[str, list[str]]:
        """Taxonomy groups and their member names — the `@group` fan-out
        spelling shared by deploy, ansible `-l`, and `deployBatch`."""
        try:
            return get_inv().groups
        except InventoryError as e:
            raise ToolError(str(e)) from e

    @mcp.tool
    def resolve(selector: str) -> dict:
        """Expand a selector (`@group`, a member, or a comma-list) —
        identical to `mandala resolve` and the `--limit` set a deploy would
        fan out to. Returns the sorted `members` plus the comma-joined
        `limit` string, which is exactly the `confirm` value the gated
        actions (deploy, reboot, restart_service) require."""
        inv = get_inv()
        try:
            return {
                "members": inv.resolve(selector),
                "limit": inv.to_limit(selector),
            }
        except InventoryError as e:
            raise ToolError(str(e)) from e

    @mcp.tool
    def ping(selector: str, forks: int = 15, connect_timeout: int = 10) -> dict:
        """Ansible reachability probe over a resolved selector — per-host
        reachable/unreachable. Read-only: it changes no fleet state. The raw
        ansible output is included so an unreachable host can be debugged.
        `forks` probes hosts concurrently and `connect_timeout` (seconds)
        caps each ssh attempt, so a whole-fleet probe with a few dead
        workstations finishes inside a client's tool-call budget instead
        of serially waiting out every unreachable host."""
        try:
            limit = get_inv().to_limit(selector)
        except InventoryError as e:
            raise ToolError(str(e)) from e
        argv = [
            "ansible", limit, "-m", "ping", "-o",
            "-f", str(max(1, forks)),
            "-T", str(max(1, connect_timeout)),
        ]
        try:
            proc = subprocess.run(
                argv, cwd=ansible_dir(), env=_adhoc_env(),
                capture_output=True, text=True,
            )
        except FileNotFoundError as e:
            raise ToolError("ansible not found on PATH") from e
        # Oneline format: "host | SUCCESS => {...}" / "host | UNREACHABLE! => …".
        # Parse regardless of exit code — a partial probe (some hosts down) is
        # the useful signal, and ansible returns non-zero whenever any host is
        # unreachable.
        reachable: dict[str, bool] = {}
        for line in proc.stdout.splitlines():
            if "|" not in line:
                continue
            host, _, rest = line.partition("|")
            host = host.strip()
            if not host:
                continue
            token = rest.strip().split(" ", 1)[0].rstrip("!:")
            reachable[host] = token == "SUCCESS"
        result = {
            "limit": limit,
            "reachable": reachable,
            "exit_code": proc.returncode,
            # stdout only: stderr rides separately so warnings and side-band
            # noise (ansible relabels any subprocess stderr, e.g. git fetch
            # progress from an inventory eval, as [ERROR]) can't masquerade
            # as probe failures.
            "output": proc.stdout.strip(),
        }
        stderr = proc.stderr.strip()
        if stderr:
            result["diagnostics"] = stderr
        return result

    @mcp.tool
    def host_eval(member: str, toplevel: bool = False) -> dict:
        """Per-host eval information. Aggregate metadata is always returned;
        the evaluated system `toplevel` out-path is computed only when
        `toplevel=true` (one slow nix eval). A failed eval is returned as a
        structured `eval_error`, not raised, so the client can debug it."""
        inv = get_inv()
        try:
            roster = inv.members
        except InventoryError as e:
            raise ToolError(str(e)) from e
        if member not in roster:
            raise ToolError(f"no such member: {member}")
        result: dict = {"member": member, "metadata": roster[member], "toplevel": None}
        if toplevel:
            try:
                evaluated = drift_mod.eval_expected(inv.flake, [member])
                result["toplevel"] = evaluated.get(member)
            except subprocess.CalledProcessError as e:
                result["eval_error"] = from_called_process(
                    f"toplevel eval failed for {member}", e
                )
        return result

    @mcp.tool
    def drift(
        refresh: bool = False,
        do_eval: bool = False,
        statuses: list[str] | None = None,
    ) -> dict:
        """Deployed-generation drift (contract vs reported state) — the same
        exact-out-path judgement `mandala drift` makes. A plain read uses
        existing snapshots and the rev-keyed expectation cache: no survey, no
        eval. `refresh` runs the read-only state survey first; `do_eval`
        re-evaluates the expected toplevels (one slow nix eval). An eval
        failure is returned as a structured `eval_error`, not raised.

        The result always carries a `summary` ({status: count} over the whole
        fleet) and `total`; `statuses` filters the `entries` list to just
        those statuses (e.g. `["drift", "unreachable"]`) so one noisy status
        — every host goes `reboot-pending` after a kernel bump — doesn't
        drown the rest. `reboot-pending` fires only on a boot-critical
        change between booted and current (kernel, kernel-modules, initrd,
        kernel-params); an activated-but-unrebooted generation with none
        of those reports `activated` instead. `expected_source: "none"` means NO expected-toplevel
        comparison happened (no cache for this rev and `do_eval` false):
        current-vs-expected judgements are then absent, not clean."""
        inv = get_inv()
        try:
            nodes = (
                inv.aggregate.get("projections", {}).get("deploy", {}).get("nodes", [])
            )
        except InventoryError as e:
            raise ToolError(str(e)) from e

        result: dict = {"refreshed": False, "expected_source": "none"}

        if refresh:
            rc = drift_mod.refresh_snapshots(ansible_dir())
            result["refreshed"] = True
            result["survey_rc"] = rc

        rev = drift_mod.repo_rev(inv.flake)
        cached_rev, cached = drift_mod.load_expected()
        expected = None
        if do_eval:
            try:
                expected = drift_mod.eval_expected(inv.flake, nodes)
                drift_mod.save_expected(rev, expected)
                result["expected_source"] = "eval"
            except subprocess.CalledProcessError as e:
                result["eval_error"] = from_called_process(
                    "expected-toplevel eval failed", e
                )
        elif drift_mod.cache_fresh(cached_rev, rev):
            expected = cached
            result["expected_source"] = "cache"

        entries = drift_mod.compare(nodes, drift_mod.read_snapshots(), expected)
        result["rev"] = rev
        dicts = [{**asdict(e), "status": e.status.value} for e in entries]
        result["summary"] = dict(Counter(d["status"] for d in dicts))
        result["total"] = len(dicts)
        if statuses:
            wanted = set(statuses)
            dicts = [d for d in dicts if d["status"] in wanted]
        result["entries"] = dicts
        return result

    @mcp.tool
    def reload() -> dict:
        """Re-read the fleet contract: evaluate a FRESH inventory aggregate
        (the one slow `nix eval .#mandala`) and swap it in as what every
        other tool serves. Use after the contract changes — a member added,
        tags moved, groups reshaped — because the aggregate is otherwise
        cached for the life of the server. Hosted in the TUI, the swap
        refreshes the operator's tables too, exactly like pressing `r`."""
        if set_inventory is None:
            raise ToolError("reload unavailable: this host cannot swap the inventory")
        fresh = Inventory(flake=get_inv().flake)
        try:
            roster = fresh.members  # force the slow aggregate eval HERE
            n_groups = len(fresh.groups)
        except InventoryError as e:
            raise ToolError(str(e)) from e
        set_inventory(fresh)
        return {"ok": True, "members": len(roster), "groups": n_groups}

    # -- monitoring + action tiers ------------------------------------

    def _run_snapshot(obs) -> dict:
        """Per-host states + build progress for one registry run. A
        failed/rolled-back host carries its raw stream so the client can
        debug it (the same text the operator reads in the failed host tab).
        A command run (reboot, …) has no event streams: its snapshot is
        liveness (pid, then the reaped rc) plus the tail of its teed
        output.log."""
        if obs.info.kind != "deploy":
            return _command_snapshot(obs)
        obs.poll()
        hosts = {}
        for name, h in obs.tailer.hosts.items():
            entry = {
                "state": h.state.value,
                "rc": h.rc,
                "milestones": list(h.milestones),
            }
            if h.state in (HostState.FAILED, HostState.ROLLED_BACK):
                entry["raw"] = list(h.lines)
            hosts[name] = entry
        b = obs.tailer.build
        liveness = obs.liveness()
        # A coarse phase so an early snapshot doesn't read as stalled: the
        # deploy playbook batch-builds every profile FIRST (play 1, no host
        # events yet), then fans out per host.
        if liveness is not RunLiveness.RUNNING:
            phase = "done"
        elif not hosts:
            phase = "batch-build"
        else:
            phase = "fan-out"
        return {
            "run_id": obs.info.run_id,
            "kind": obs.info.kind,
            "meta": obs.info.meta,
            "liveness": liveness.value,
            "phase": phase,
            "hosts": hosts,
            "build": {
                "built": b.built,
                "finished": b.finished,
                "fetched": b.fetched,
                "errors": b.errors,
                "done": b.done,
                "rc": b.rc,
            },
        }

    def _command_snapshot(obs, tail: int = 120) -> dict:
        liveness = obs.liveness()
        log_path = obs.info.path / COMMAND_LOG
        try:
            lines = log_path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            lines = []
        return {
            "run_id": obs.info.run_id,
            "kind": obs.info.kind,
            "meta": obs.info.meta,
            "liveness": liveness.value,
            "phase": "running" if liveness is RunLiveness.RUNNING else "done",
            "log": str(log_path),
            "output_tail": lines[-tail:],
        }

    @mcp.tool
    def deploy_status(
        run_id: str | None = None, limit: int = 10, wait_seconds: int = 0
    ) -> dict:
        """Live and recent run state from the shared run registry — EVERY
        registered run kind (deploys and command runs like reboot), from
        any frontend (TUI, CLI, or this server). With a `run_id`, report
        just that run; otherwise the most recent `limit` runs — which also
        surfaces orphaned/lingering runs (`liveness: running` with an old
        `started_at`). Deploy runs report per-host states from the
        protocol's sticky terminal states, so a confirmed host stays
        confirmed and a rolled-back host stays flagged; `milestones` is the
        raw per-host event sequence — repeats (activate, wait, activate, …)
        are genuine re-entries from the engine, not display noise. `phase`
        summarizes where a live deploy is: `batch-build` (play 1, no
        per-host events yet) → `fan-out` → `done`. Command runs report
        liveness (pid, then the reaped exit code in `meta.rc`) plus the
        tail of their teed `output.log`.

        `wait_seconds` (with a `run_id`) blocks until the run leaves the
        `running` state or the wait elapses — one call instead of a poll
        loop; capped at 570s to stay under client timeouts. The returned
        `liveness` tells whether it finished or the wait timed out."""
        from .. import registry

        if run_id is not None:
            obs = registry.open_run(run_id)
            if obs is None:
                raise ToolError(f"no such run: {run_id}")
            snap = _run_snapshot(obs)
            deadline = time.monotonic() + min(max(wait_seconds, 0), _MAX_WAIT_SECONDS)
            while (
                snap["liveness"] == RunLiveness.RUNNING.value
                and time.monotonic() < deadline
            ):
                time.sleep(2)
                snap = _run_snapshot(obs)
            return snap
        runs = []
        for info in registry.list_runs()[: max(1, limit)]:
            obs = registry.open_run(info.run_id)
            if obs is not None:
                runs.append(_run_snapshot(obs))
        return {"runs": runs}

    @mcp.tool
    def build(selector: str, wait_seconds: int = 120) -> dict:
        """Build the resolved members' system `toplevel`(s) with `nix build` —
        WITHOUT activating anything (local store only), so no confirmation is
        required. Launches as a REGISTERED BACKGROUND RUN (a cold multi-host
        build outlives any client timeout) and waits up to `wait_seconds`
        (cap 570) for it to finish: a finished build returns `ok` +
        `out_paths` (or the failing output), a still-running one returns
        `building: true` — follow with `deploy_status(run_id,
        wait_seconds=…)`. Output streams to the returned `log` either way."""
        inv = get_inv()
        try:
            targets = inv.resolve(selector)
        except InventoryError as e:
            raise ToolError(str(e)) from e
        installables = [
            f"{inv.flake}#nixosConfigurations.{m}.config.system.build.toplevel"
            for m in targets
        ]
        argv = [
            "nix", "build", "--no-link", "--print-out-paths",
            "--no-warn-dirty", *installables,
        ]
        from .. import registry
        from ..runner import CommandRun

        run = CommandRun(argv=argv, kind="build", extra_meta={"members": targets})
        run.start()
        result = {
            "run_id": run.run_id,
            "members": targets,
            "log": str(run.log_path),
        }
        if not run.launched:
            return {**result, "ok": False, "error": "failed to launch nix — see log"}
        obs = registry.open_run(run.run_id)
        deadline = time.monotonic() + min(max(wait_seconds, 0), _MAX_WAIT_SECONDS)
        while obs.liveness() is RunLiveness.RUNNING and time.monotonic() < deadline:
            time.sleep(1)
        if obs.liveness() is RunLiveness.RUNNING:
            return {**result, "building": True}
        rc = obs.info.meta.get("rc")
        try:
            lines = (run.log_path).read_text(
                encoding="utf-8", errors="replace"
            ).splitlines()
        except OSError:
            lines = []
        if rc != 0:
            return {
                **result, "ok": False, "exit_code": rc,
                "error": "nix build failed",
                "output": "\n".join(lines[-80:]),
            }
        # The teed log interleaves nix's stderr chatter with the printed
        # out-paths; the out-paths are the unindented store paths.
        out_paths = [l for l in lines if l.startswith("/nix/store/")]
        return {**result, "ok": True, "out_paths": out_paths}

    @mcp.tool
    def deploy(
        selector: str, dry_activate: bool = True, confirm: str | None = None
    ) -> dict:
        """Deploy the resolved members through the deploy playbook — the
        engine: `--limit`, throttle, and deploy-rs magic rollback are never
        bypassed. Defaults to dry-activate (build + copy, no switch). A REAL
        activation (`dry_activate=false`) requires `confirm` to equal the
        resolved `--limit` target — take it from `resolve`'s `limit` field, a
        prior run's `limit`, or this tool's refusal (`required_confirm`) —
        else it refuses WITHOUT launching. Returns the run id; follow with
        `deploy_status` (its `wait_seconds` blocks until the run settles)."""
        try:
            target = get_inv().to_limit(selector)
        except InventoryError as e:
            raise ToolError(str(e)) from e
        if not dry_activate and confirm != target:
            return {
                "ok": False,
                "refused": True,
                "reason": "real activation requires `confirm` to equal the resolved target",
                "required_confirm": target,
                "dry_activate": dry_activate,
            }
        from ..runner import DeployRun

        run = DeployRun(limit=target, dry_activate=dry_activate)
        run.start()
        return {
            "ok": True,
            "run_id": run.run_id,
            "limit": target,
            "dry_activate": dry_activate,
            "events_dir": str(run.events_dir),
        }

    @mcp.tool
    def restart_service(
        selector: str,
        unit: str,
        confirm: str | None = None,
        forks: int = 4,
    ) -> dict:
        """Restart one systemd unit on the resolved members via ad-hoc
        ansible (`systemd_service state=restarted`) — the middle ground
        between a full deploy (no-op when the closure hasn't changed) and a
        reboot (far too big a hammer for picking up a service-level change,
        e.g. k3s re-reading registries.yaml). `forks` bounds how many hosts
        restart AT ONCE — a concurrency cap, NOT a rolling gate: there is
        no fail-fast or health check between batches, so every resolved
        host is eventually restarted even if the first batch breaks.

        Mutating, so it takes the deploy/reboot confirm gate: `confirm` must
        equal the resolved `--limit` target (the `limit` field of `resolve`),
        else it refuses WITHOUT running. Unit names are validated to a plain
        systemd name — no paths, no shell."""
        try:
            target = get_inv().to_limit(selector)
        except InventoryError as e:
            raise ToolError(str(e)) from e
        if not _UNIT_RE.match(unit):
            raise ToolError(f"not a plain systemd unit name: {unit!r}")
        if confirm != target:
            return {
                "ok": False,
                "refused": True,
                "reason": "restart_service requires `confirm` to equal the resolved target",
                "required_confirm": target,
                "unit": unit,
            }
        argv = [
            "ansible", target,
            "-m", "ansible.builtin.systemd_service",
            "-a", f"name={unit} state=restarted",
            "-f", str(max(1, forks)),
        ]
        try:
            proc = subprocess.run(
                argv, cwd=ansible_dir(), env=_adhoc_env(),
                capture_output=True, text=True,
            )
        except FileNotFoundError as e:
            raise ToolError("ansible not found on PATH") from e
        # Ad-hoc result lines: "host | CHANGED => {..." / "host | FAILED! => …";
        # parse regardless of exit code — the per-host map is the signal.
        restarted: dict[str, bool] = {}
        for line in proc.stdout.splitlines():
            m = re.match(r"^(\S+) \| ([A-Z]+)", line)
            if m:
                restarted[m.group(1)] = m.group(2) in ("CHANGED", "SUCCESS")
        result = {
            "ok": proc.returncode == 0,
            "limit": target,
            "unit": unit,
            "restarted": restarted,
            "exit_code": proc.returncode,
        }
        if proc.returncode != 0:
            result["output"] = proc.stdout.strip()
        stderr = proc.stderr.strip()
        if stderr:
            result["diagnostics"] = stderr
        return result

    @mcp.tool
    def reboot(
        selector: str,
        serial: str = "1",
        drain: bool = True,
        confirm: str | None = None,
    ) -> dict:
        """Reboot the resolved members through the reboot playbook with the
        serial-order (`1` serial, `2` rolling, `100%` all-at-once) and k8s
        `drain` options the TUI offers. Requires `confirm` to equal the
        resolved `--limit` target, else it refuses WITHOUT launching. Prefers
        the operator's `ans-reboot` wrapper (which carries the env the k8s
        drain needs), falling back to `playbooks/reboot.yaml`.

        Launches as a REGISTERED BACKGROUND RUN and returns `run_id` at
        once — a rolling reboot far outlives any client timeout, and this
        way the run stays observable (TUI, `deploy_status`) instead of
        orphaning. Follow with `deploy_status(run_id, wait_seconds=…)`;
        the playbook output streams to the returned `log` file either way."""
        try:
            target = get_inv().to_limit(selector)
        except InventoryError as e:
            raise ToolError(str(e)) from e
        if not _SERIAL_RE.match(serial):
            # `-e "a=1 b=2"` sets MULTIPLE extra-vars — refuse anything but
            # a plain batch count / percentage before ansible parses it.
            raise ToolError(f"not a serial batch count or percentage: {serial!r}")
        if confirm != target:
            return {
                "ok": False,
                "refused": True,
                "reason": "reboot requires `confirm` to equal the resolved target",
                "required_confirm": target,
            }
        argv = reboot_argv(target, serial, drain)
        if argv is None:
            raise ToolError(
                "no ans-reboot wrapper or playbooks/reboot.yaml — reboot unavailable"
            )
        from ..runner import CommandRun

        run = CommandRun(
            argv=argv,
            kind="reboot",
            cwd=ansible_dir(),
            extra_meta={"limit": target, "serial": serial, "drain": drain},
        )
        run.start()
        if not run.launched:
            return {
                "ok": False,
                "error": f"failed to launch {argv[0]} — see log",
                "run_id": run.run_id,
                "log": str(run.log_path),
            }
        return {
            "ok": True,
            "run_id": run.run_id,
            "limit": target,
            "serial": serial,
            "drain": drain,
            "log": str(run.log_path),
        }

    return mcp
