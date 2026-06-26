"""Embedded HTTP host: serve the FastMCP server in the caller's event loop,
guarded by a loopback bind + a bearer token.

FastMCP's OAuth/JWT auth providers are overkill for a single-operator
loopback endpoint, so the token is enforced by a tiny ASGI middleware that
rejects any HTTP request without the matching `Authorization: Bearer`
header (lifespan and other scopes pass straight through, so the FastMCP
session manager still initializes). The app is run with uvicorn inside the
current loop, so a tool call renders in the same process the operator is
watching.
"""

from __future__ import annotations

from .server import build_server

DEFAULT_PORT = 7878


class _TokenASGIMiddleware:
    """Reject any HTTP request lacking the session bearer token. Non-HTTP
    scopes (notably `lifespan`) pass through untouched."""

    def __init__(self, app, token: str) -> None:
        self.app = app
        self.token = token

    async def __call__(self, scope, receive, send):  # type: ignore[no-untyped-def]
        if scope.get("type") == "http":
            headers = dict(scope.get("headers") or [])
            if headers.get(b"authorization", b"").decode() != f"Bearer {self.token}":
                await send({
                    "type": "http.response.start",
                    "status": 401,
                    "headers": [(b"content-type", b"text/plain")],
                })
                await send({"type": "http.response.body", "body": b"unauthorized"})
                return
        await self.app(scope, receive, send)


async def serve_http(
    inv,
    *,
    token: str,
    host: str = "127.0.0.1",
    port: int = DEFAULT_PORT,
    path: str = "/mcp",
    activity_sink=None,
) -> None:
    """Build the server and run its HTTP transport (token-guarded) in the
    current event loop until cancelled."""
    import uvicorn

    server = build_server(inv, activity_sink=activity_sink)
    app = _TokenASGIMiddleware(server.http_app(path=path), token)
    config = uvicorn.Config(
        app, host=host, port=port, log_level="warning", lifespan="on"
    )
    await uvicorn.Server(config).serve()
