"""Built-in deploy engine: dispatch onto the mandala.fleet fan-out.

Dispatch + present only — deploy-rs and the mandala.fleet ansible
collection remain the machinery. `run` shells out to the fan-out playbook
(eval-once batch build + per-host deploy-rs); `batch` builds a
deployBatch group artifact for cache warming.
"""

from __future__ import annotations

import os
import subprocess

import typer

from ..inventory import Inventory, InventoryError

app = typer.Typer(no_args_is_help=True, help="Fan-out deploys via deploy-rs + mandala.fleet")


@app.command()
def run(
    ctx: typer.Context,
    limit: str = typer.Option(..., "--limit", "-l", help="Hosts/groups for ansible --limit (required by the playbook's guard)"),
    dry_activate: bool = typer.Option(False, help="Build + copy but do not activate"),
    throttle: int = typer.Option(4, help="Per-host deploy parallelism"),
    events_dir: str = typer.Option(None, "--events-dir", help="Opt into the JSONL event channel (MANDALA_FLEET_EVENTS)"),
) -> None:
    """Run the eval-once + fan-out deploy (mandala.fleet.deploy)."""
    env = dict(os.environ)
    if events_dir:
        env["MANDALA_FLEET_EVENTS"] = events_dir
    argv = [
        "ansible-playbook", "mandala.fleet.deploy",
        "-l", limit,
        "-e", f"deploy_throttle={throttle}",
    ]
    if dry_activate:
        argv += ["-e", "deploy_dry_activate=true"]
    raise typer.Exit(subprocess.run(argv, env=env, check=False).returncode)


@app.command()
def batch(
    ctx: typer.Context,
    group: str = typer.Argument(..., help="deployBatch group key (taxonomy spelling)"),
) -> None:
    """Build a group's eval-once batch artifact (.#deployBatch.<group>)."""
    inv: Inventory = ctx.obj
    if group != "all" and group not in inv.groups:
        raise InventoryError(f"no such group: {group}")
    argv = ["nix", "build", "--no-link", "--print-out-paths", f"{inv.flake}#deployBatch.{group}"]
    raise typer.Exit(subprocess.run(argv, check=False).returncode)


@app.command()
def nodes(ctx: typer.Context) -> None:
    """List deploy-rs node names (from the aggregate's deploy projection)."""
    inv: Inventory = ctx.obj
    deploy = inv.aggregate.get("projections", {}).get("deploy", {})
    for name in sorted(deploy.get("nodes", [])):
        typer.echo(name)
