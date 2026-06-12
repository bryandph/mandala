"""Built-in ansible engine: views over the inventory projection."""

from __future__ import annotations

import json

import typer

from ..inventory import Inventory

app = typer.Typer(no_args_is_help=True, help="Views over the projected ansible inventory")


@app.command()
def inventory(ctx: typer.Context) -> None:
    """Print the projected ansible inventory (the dynamic-inventory data)."""
    inv: Inventory = ctx.obj
    projected = inv.aggregate.get("projections", {}).get("ansibleInventory")
    if projected is None:
        typer.echo("no ansibleInventory projection in the aggregate (import the ansible flakeModule)", err=True)
        raise typer.Exit(1)
    typer.echo(json.dumps(projected, indent=2, sort_keys=True))
