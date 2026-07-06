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

import asyncio
import hmac

from .server import build_server

DEFAULT_PORT = 7878


class _TokenASGIMiddleware:
    """Reject any HTTP request lacking the session bearer token. Non-HTTP
    scopes (notably `lifespan`) pass through untouched."""

    def __init__(self, app, token: str) -> None:
        self.app = app
        self._expect = f"Bearer {token}".encode()

    async def __call__(self, scope, receive, send):  # type: ignore[no-untyped-def]
        if scope.get("type") == "http":
            headers = dict(scope.get("headers") or [])
            supplied = headers.get(b"authorization", b"")
            # Bytes-vs-bytes and constant-time: a malformed header must
            # 401 (not decode-crash to a 500), and the comparison must not
            # leak the token by timing.
            if not hmac.compare_digest(supplied, self._expect):
                await send({
                    "type": "http.response.start",
                    "status": 401,
                    "headers": [(b"content-type", b"text/plain")],
                })
                await send({"type": "http.response.body", "body": b"unauthorized"})
                return
        await self.app(scope, receive, send)


async def serve_http(
    inventory,
    *,
    token: str,
    host: str = "127.0.0.1",
    port: int = DEFAULT_PORT,
    path: str = "/mcp",
    activity_sink=None,
    set_inventory=None,
    shutdown: asyncio.Event | None = None,
) -> None:
    """Build the server and run its HTTP transport (token-guarded) in the
    current event loop.

    `shutdown`, when given, is the ORDERLY exit path: setting it makes
    uvicorn stop accepting, drain in-flight requests, and close the
    streamable-http lifespan in order. An abrupt task cancel instead tears
    the session manager's task group down while the socket still accepts,
    so late client retries spew "Task group is not initialized"."""
    import uvicorn

    server = build_server(
        inventory, activity_sink=activity_sink, set_inventory=set_inventory
    )
    app = _TokenASGIMiddleware(server.http_app(path=path), token)
    config = uvicorn.Config(
        app, host=host, port=port, log_level="warning", lifespan="on",
        timeout_graceful_shutdown=2,
    )
    uv = uvicorn.Server(config)
    if shutdown is None:
        await uv.serve()
        return
    serve_task = asyncio.create_task(uv.serve())
    wait_task = asyncio.create_task(shutdown.wait())
    try:
        done, _ = await asyncio.wait(
            {serve_task, wait_task}, return_when=asyncio.FIRST_COMPLETED
        )
    except asyncio.CancelledError:
        # Abrupt cancel (not the graceful `shutdown` path): don't leave the
        # serve task running detached until the loop closes.
        wait_task.cancel()
        serve_task.cancel()
        raise
    if serve_task in done:
        wait_task.cancel()
        await serve_task  # propagate a bind/serve failure to the caller
        return
    uv.should_exit = True
    await serve_task
