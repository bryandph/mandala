"""Typer root: fleet views + engine discovery.

Engines (built-in AND operator plugins) are Typer sub-apps found through
the `mandala.engines` entry-point group — `mandala <engine> ...` is the
whole dispatch story. The root app only owns the fleet views that come
straight from the inventory core.
"""

from __future__ import annotations

import json
import sys
from importlib.metadata import entry_points

import typer

from . import __version__
from .inventory import Inventory, InventoryError

app = typer.Typer(
    no_args_is_help=True,
    help="mandala fleet porcelain — engines plug in via `mandala.engines` entry points",
)


@app.callback()
def _root(
    ctx: typer.Context,
    flake: str = typer.Option(".", "--flake", "-f", help="Flake exposing the mandala aggregate"),
) -> None:
    ctx.obj = Inventory(flake=flake)


@app.command()
def members(ctx: typer.Context, as_json: bool = typer.Option(False, "--json")) -> None:
    """List the merged member view (NixOS + facts-only members)."""
    inv: Inventory = ctx.obj
    if as_json:
        typer.echo(json.dumps(inv.members, indent=2, sort_keys=True))
        return
    from rich import box
    from rich.console import Console
    from rich.table import Table

    from .inventory import surfaces

    table = Table(box=box.SIMPLE_HEAD, header_style="bold", pad_edge=False)
    table.add_column("member", style="bold")
    table.add_column("platform")
    table.add_column("arch", style="dim")
    table.add_column("category", style="dim")
    table.add_column("role")
    table.add_column("tags", style="dim", overflow="fold")
    table.add_column("ads", style="cyan")
    for name, m in sorted(inv.members.items()):
        table.add_row(
            name,
            m.get("platform", "?"),
            m.get("architecture", "?"),
            m.get("category", "?"),
            m.get("role") or "-",
            " ".join(m.get("tags", [])),
            surfaces(m),
        )
    table.caption = f"{len(inv.members)} members — ads = ansible/deploy-rs/sops"
    Console().print(table)


@app.command()
def groups(ctx: typer.Context, as_json: bool = typer.Option(False, "--json")) -> None:
    """List taxonomy groups and their members (the fan-out spelling)."""
    inv: Inventory = ctx.obj
    if as_json:
        typer.echo(json.dumps(inv.groups, indent=2, sort_keys=True))
        return
    from rich import box
    from rich.console import Console
    from rich.table import Table

    table = Table(box=box.SIMPLE_HEAD, header_style="bold", pad_edge=False)
    table.add_column("group", style="bold")
    table.add_column("n", justify="right", style="cyan")
    table.add_column("members", overflow="fold", style="dim")
    for group, names in sorted(inv.groups.items()):
        table.add_row(group, str(len(names)), " ".join(sorted(names)))
    table.caption = f"{len(inv.groups)} groups — one spelling: @group, ansible -l, deployBatch"
    Console().print(table)


@app.command()
def resolve(ctx: typer.Context, selector: str) -> None:
    """Expand a selector (`@group`, member, comma-list) to member names."""
    inv: Inventory = ctx.obj
    for name in inv.resolve(selector):
        typer.echo(name)


@app.command()
def drift(
    ctx: typer.Context,
    do_eval: bool = typer.Option(False, "--eval", help="Evaluate expected toplevels (one slow nix eval)"),
    refresh: bool = typer.Option(False, "--refresh", help="Run the read-only state survey (mandala.fleet.state) first"),
) -> None:
    """Deployed-generation drift: contract vs reported fleet state."""
    from pathlib import Path

    from . import drift as drift_mod

    inv: Inventory = ctx.obj
    nodes = inv.aggregate.get("projections", {}).get("deploy", {}).get("nodes", [])
    if refresh:
        ansible_dir = Path("ansible") if Path("ansible/ansible.cfg").is_file() else Path(".")
        rc = drift_mod.refresh_snapshots(ansible_dir)
        if rc != 0:
            typer.echo(f"state survey exited {rc} (continuing with whatever was captured)", err=True)

    # Expected toplevels: re-evaluated on --eval, else reused from the
    # rev-keyed cache when the contract hasn't moved since the last eval.
    rev = drift_mod.repo_rev(inv.flake)
    cached_rev, cached = drift_mod.load_expected()
    expected = None
    if do_eval:
        import subprocess

        try:
            expected = drift_mod.eval_expected(inv.flake, nodes)
        except subprocess.CalledProcessError as e:
            tail = (e.stderr or "").strip().splitlines()[-8:]
            typer.echo("expected-toplevel eval failed:", err=True)
            for line in tail:
                typer.echo(f"  {line}", err=True)
            raise typer.Exit(1) from e
        drift_mod.save_expected(rev, expected)
    elif drift_mod.cache_fresh(cached_rev, rev):
        expected = cached
        typer.echo(f"(expected from cache @ {rev[:11]})", err=True)

    entries = drift_mod.compare(nodes, drift_mod.read_snapshots(), expected)
    short = lambda p: (p or "-").removeprefix("/nix/store/")[:20]
    for e in entries:
        typer.echo(f"{e.host}\t{e.status.value}\t{short(e.current)}\t{short(e.expected)}")
    if expected is None:
        if cached_rev is not None:
            typer.echo(
                f"(expected cache stale: evaluated @ {cached_rev[:11]}, repo now @ "
                f"{(rev or '?')[:11]} — the contract moved; pass --eval)",
                err=True,
            )
        else:
            typer.echo("(expected not evaluated — pass --eval for real drift judgement)", err=True)


@app.command()
def version() -> None:
    """Print the CLI version."""
    typer.echo(__version__)


tui_app = typer.Typer(
    help="Textual TUI tiers: read-only explorer + drift dashboard; deploy runner",
    invoke_without_command=True,
)


@tui_app.callback(invoke_without_command=True)
def tui_main(ctx: typer.Context) -> None:
    """`mandala tui` opens the read-only fleet explorer."""
    if ctx.invoked_subcommand is not None:
        return
    from .tui.explorer import ExplorerApp

    ExplorerApp(ctx.obj).run()


@tui_app.command("deploy")
def tui_deploy(
    ctx: typer.Context,
    limit: str = typer.Option(..., "--limit", "-l", help="Selector: @group, member, or comma-list"),
    dry_activate: bool = typer.Option(False, help="Build + copy but do not activate"),
    throttle: int = typer.Option(4, help="Per-host deploy parallelism"),
) -> None:
    """Deploy-runner view: the fan-out playbook, presented live."""
    from .runner import DeployRun
    from .tui.deploy import DeployApp

    inv: Inventory = ctx.obj
    run = DeployRun(
        limit=inv.to_limit(limit),
        dry_activate=dry_activate,
        throttle=throttle,
    )
    raise typer.Exit(DeployApp(run).run() or 0)


app.add_typer(tui_app, name="tui")


def _load_engines(root: typer.Typer) -> None:
    """Attach every `mandala.engines` entry point as a sub-app.

    A broken plugin must not take the whole CLI down: load failures are
    reported on stderr and the engine is skipped.
    """
    for ep in entry_points(group="mandala.engines"):
        try:
            engine = ep.load()
        except Exception as e:  # noqa: BLE001 — plugin isolation boundary
            print(f"mandala: skipping engine '{ep.name}': {e}", file=sys.stderr)
            continue
        root.add_typer(engine, name=ep.name)


_load_engines(app)


def main() -> None:
    app()
