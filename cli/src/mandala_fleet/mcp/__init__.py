"""Fleet MCP server: the AI-operator porcelain over the mandala cores.

A third presenter beside the CLI and the TUI — for an AI operator instead
of a human one. `build_server(inv)` returns a transport-agnostic FastMCP
instance; the caller runs it over stdio (`mandala mcp`) or co-runs its HTTP
app inside the TUI's event loop (`mandala tui --mcp`). It reuses the
inventory/drift/runner cores and adds no orchestration of its own.
"""

from .server import build_server

__all__ = ["build_server"]
