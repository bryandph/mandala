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
    for name, member in sorted(inv.members.items()):
        platform = member.get("platform", "?")
        role = member.get("role") or "-"
        typer.echo(f"{name}\t{platform}\t{role}")


@app.command()
def groups(ctx: typer.Context, as_json: bool = typer.Option(False, "--json")) -> None:
    """List taxonomy groups and their members (the fan-out spelling)."""
    inv: Inventory = ctx.obj
    if as_json:
        typer.echo(json.dumps(inv.groups, indent=2, sort_keys=True))
        return
    for group, names in sorted(inv.groups.items()):
        typer.echo(f"{group}\t{len(names)}\t{' '.join(sorted(names))}")


@app.command()
def version() -> None:
    """Print the CLI version."""
    typer.echo(__version__)


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
