"""Typer root: the TUI-only Python surface.

The headless surfaces (root fleet views, the deploy/ansible engines, and
the stdio MCP server) moved to the Rust porcelain (`mandala-core` + the
operator bin crate); this package remains as `mandala-py` serving the
Textual TUI tiers — the read-only explorer (optionally hosting the HTTP
MCP endpoint) and the deploy runner — until the rewrite's phase 2.
"""

from __future__ import annotations

import typer

from .inventory import Inventory

app = typer.Typer(
    no_args_is_help=True,
    help="mandala fleet porcelain (Python) — TUI tiers only; the headless CLI/MCP live in the Rust binary",
)


@app.callback()
def _root(
    ctx: typer.Context,
    flake: str = typer.Option(".", "--flake", "-f", help="Flake exposing the mandala aggregate"),
) -> None:
    ctx.obj = Inventory(flake=flake)


tui_app = typer.Typer(
    help="Textual TUI tiers: read-only explorer + drift dashboard; deploy runner",
    invoke_without_command=True,
)


@tui_app.callback(invoke_without_command=True)
def tui_main(
    ctx: typer.Context,
    mcp: bool = typer.Option(
        False, "--mcp", help="Host the fleet MCP server (loopback HTTP) and show a live Claude-activity pane"
    ),
    mcp_port: int = typer.Option(
        7878, "--mcp-port", help="Port for the embedded MCP HTTP endpoint"
    ),
    mcp_rotate_token: bool = typer.Option(
        False, "--mcp-rotate-token", help="Mint a fresh bearer token before serving"
    ),
) -> None:
    """`mandala tui` opens the read-only fleet explorer.

    With `--mcp` it also hosts the fleet MCP server over a loopback HTTP
    endpoint (bearer-token guarded) so an AI operator can drive the fleet
    while you watch every call in the activity pane."""
    if ctx.invoked_subcommand is not None:
        return
    from .tui.explorer import ExplorerApp

    ExplorerApp(
        ctx.obj,
        serve_mcp=mcp,
        mcp_port=mcp_port,
        mcp_rotate_token=mcp_rotate_token,
    ).run()


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


def main() -> None:
    app()
