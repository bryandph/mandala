"""FastMCP server over the fleet cores — read + drift surface (the action
tiers are added in a later increment).

Pure presentation: every tool delegates to the inventory/drift cores, the
same ones the CLI and TUI read, so a selector here resolves to exactly what
`mandala resolve` and `ansible -l` project. The slow inputs (a host's
`toplevel` eval, the drift survey, the expected-toplevel eval) run only when
a tool argument explicitly asks for them, mirroring the CLI's opt-ins.
"""

from __future__ import annotations

import subprocess
from dataclasses import asdict
from pathlib import Path

from fastmcp import FastMCP
from fastmcp.exceptions import ToolError

from .. import drift as drift_mod
from ..inventory import Inventory, InventoryError
from ..runner import HostState
from .errors import from_called_process, from_completed


def _ansible_dir() -> Path:
    return Path("ansible") if Path("ansible/ansible.cfg").is_file() else Path(".")


def build_server(inv: Inventory, activity_sink=None) -> FastMCP:
    """A transport-agnostic FastMCP server over the inventory. Registering
    tools never evaluates the fleet — the first read triggers the one
    `nix eval .#mandala`, gated by schemaVersion in the inventory core.

    `activity_sink`, when given, receives `{tool, args, status, detail}` per
    tool call (the TUI-hosted transport feeds this into its activity pane)."""
    mcp = FastMCP("mandala-fleet")
    if activity_sink is not None:
        from .activity import ActivityMiddleware

        mcp.add_middleware(ActivityMiddleware(activity_sink))

    @mcp.tool
    def members() -> dict[str, dict]:
        """Every fleet member (NixOS + facts-only) with its aggregate
        metadata — platform, arch, role, tags, management surfaces."""
        try:
            return inv.members
        except InventoryError as e:
            raise ToolError(str(e)) from e

    @mcp.tool
    def groups() -> dict[str, list[str]]:
        """Taxonomy groups and their member names — the `@group` fan-out
        spelling shared by deploy, ansible `-l`, and `deployBatch`."""
        try:
            return inv.groups
        except InventoryError as e:
            raise ToolError(str(e)) from e

    @mcp.tool
    def resolve(selector: str) -> list[str]:
        """Expand a selector (`@group`, a member, or a comma-list) to sorted
        member names — identical to `mandala resolve` and the `--limit` set a
        deploy would fan out to."""
        try:
            return inv.resolve(selector)
        except InventoryError as e:
            raise ToolError(str(e)) from e

    @mcp.tool
    def ping(selector: str) -> dict:
        """Ansible reachability probe over a resolved selector — per-host
        reachable/unreachable. Read-only: it changes no fleet state. The raw
        ansible output is included so an unreachable host can be debugged."""
        try:
            limit = inv.to_limit(selector)
        except InventoryError as e:
            raise ToolError(str(e)) from e
        argv = ["ansible", limit, "-m", "ping", "-o"]
        try:
            proc = subprocess.run(
                argv, cwd=_ansible_dir(), capture_output=True, text=True
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
        return {
            "limit": limit,
            "reachable": reachable,
            "exit_code": proc.returncode,
            "output": (proc.stdout + proc.stderr).strip(),
        }

    @mcp.tool
    def host_eval(member: str, toplevel: bool = False) -> dict:
        """Per-host eval information. Aggregate metadata is always returned;
        the evaluated system `toplevel` out-path is computed only when
        `toplevel=true` (one slow nix eval). A failed eval is returned as a
        structured `eval_error`, not raised, so the client can debug it."""
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
    def drift(refresh: bool = False, do_eval: bool = False) -> dict:
        """Deployed-generation drift (contract vs reported state) — the same
        exact-out-path judgement `mandala drift` makes. A plain read uses
        existing snapshots and the rev-keyed expectation cache: no survey, no
        eval. `refresh` runs the read-only state survey first; `do_eval`
        re-evaluates the expected toplevels (one slow nix eval). An eval
        failure is returned as a structured `eval_error`, not raised."""
        try:
            nodes = (
                inv.aggregate.get("projections", {}).get("deploy", {}).get("nodes", [])
            )
        except InventoryError as e:
            raise ToolError(str(e)) from e

        result: dict = {"refreshed": False, "expected_source": "none"}

        if refresh:
            rc = drift_mod.refresh_snapshots(_ansible_dir())
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
        result["entries"] = [
            {**asdict(e), "status": e.status.value} for e in entries
        ]
        return result

    # -- monitoring + action tiers ------------------------------------

    def _run_snapshot(obs) -> dict:
        """Per-host states + build progress for one registry run. A
        failed/rolled-back host carries its raw stream so the client can
        debug it (the same text the operator reads in the failed host tab)."""
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
        return {
            "run_id": obs.info.run_id,
            "meta": obs.info.meta,
            "liveness": obs.liveness().value,
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

    @mcp.tool
    def deploy_status(run_id: str | None = None, limit: int = 10) -> dict:
        """Live and recent deploy state from the shared run registry — so a
        deploy launched from any frontend (TUI, CLI, or this server) is
        observable. With a `run_id`, report just that run; otherwise the most
        recent `limit` runs. Per-host states come from the protocol's sticky
        terminal states, so a confirmed host stays confirmed and a rolled-back
        host stays flagged."""
        from .. import registry

        if run_id is not None:
            obs = registry.open_run(run_id)
            if obs is None:
                raise ToolError(f"no such run: {run_id}")
            return _run_snapshot(obs)
        runs = []
        for info in registry.list_runs()[: max(1, limit)]:
            obs = registry.open_run(info.run_id)
            if obs is not None:
                runs.append(_run_snapshot(obs))
        return {"runs": runs}

    @mcp.tool
    def build(selector: str) -> dict:
        """Build the resolved members' system `toplevel`(s) with `nix build` —
        WITHOUT activating anything (local store only), so no confirmation is
        required. A build failure is returned as a structured error with the
        nix output, so the client can debug it."""
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
        try:
            proc = subprocess.run(argv, capture_output=True, text=True)
        except FileNotFoundError as e:
            raise ToolError("nix not found on PATH") from e
        if proc.returncode != 0:
            return {"members": targets, **from_completed("nix build failed", proc)}
        out_paths = [p for p in proc.stdout.splitlines() if p.strip()]
        return {"ok": True, "members": targets, "out_paths": out_paths}

    @mcp.tool
    def deploy(
        selector: str, dry_activate: bool = True, confirm: str | None = None
    ) -> dict:
        """Deploy the resolved members through the deploy playbook — the
        engine: `--limit`, throttle, and deploy-rs magic rollback are never
        bypassed. Defaults to dry-activate (build + copy, no switch). A REAL
        activation (`dry_activate=false`) requires `confirm` to equal the
        resolved `--limit` target, else it refuses WITHOUT launching. Returns
        the run id; poll `deploy_status` for progress."""
        try:
            target = inv.to_limit(selector)
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
        drain needs), falling back to `playbooks/reboot.yaml`. Runs to
        completion; a failure returns the ansible output."""
        import shutil

        try:
            target = inv.to_limit(selector)
        except InventoryError as e:
            raise ToolError(str(e)) from e
        if confirm != target:
            return {
                "ok": False,
                "refused": True,
                "reason": "reboot requires `confirm` to equal the resolved target",
                "required_confirm": target,
            }
        ansible_dir = _ansible_dir()
        if shutil.which("ans-reboot"):
            base = ["ans-reboot", "-l", target]
        elif (ansible_dir / "playbooks/reboot.yaml").is_file():
            base = ["ansible-playbook", "playbooks/reboot.yaml", "-l", target]
        else:
            raise ToolError(
                "no ans-reboot wrapper or playbooks/reboot.yaml — reboot unavailable"
            )
        argv = base + [
            "-e", f"reboot_serial={serial}",
            "-e", f"drain={'true' if drain else 'false'}",
        ]
        try:
            proc = subprocess.run(argv, cwd=ansible_dir, capture_output=True, text=True)
        except FileNotFoundError as e:
            raise ToolError(f"{base[0]} not found on PATH") from e
        if proc.returncode != 0:
            return {"limit": target, **from_completed("reboot failed", proc)}
        return {
            "ok": True,
            "limit": target,
            "serial": serial,
            "drain": drain,
            "output": (proc.stdout + proc.stderr).strip(),
        }

    return mcp
