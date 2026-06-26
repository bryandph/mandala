"""Embedded-HTTP session discovery + the per-user bearer token.

`mandala tui --mcp` binds a loopback HTTP endpoint guarded by a bearer
token. The token is STABLE per user (minted once, reused across sessions)
so the Claude Code MCP config can carry it in a static header and every
`tui --mcp` launch just works; `--mcp-rotate-token` forces a fresh one.
The discovery file (`state_dir()/mcp/session.json`, mode 0600) records the
current `{url, token, pid}` so the operator can read the token for the
header. The file is owner-only because the token authorizes deploys.
"""

from __future__ import annotations

import json
import os
import secrets
from pathlib import Path

from ..drift import state_dir


def session_path() -> Path:
    return state_dir() / "mcp" / "session.json"


def _read() -> dict:
    try:
        return json.loads(session_path().read_text())
    except (OSError, ValueError):
        return {}


def ensure_session(url: str, *, pid: int | None = None, rotate: bool = False) -> str:
    """Return the bearer token for this session, writing the discovery file.

    Reuses the persisted token unless `rotate` is set or none exists yet.
    The file is written 0600 — the token authorizes mutating fleet actions.
    """
    existing = _read()
    token = existing.get("token")
    if rotate or not token:
        token = secrets.token_urlsafe(32)
    path = session_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    # Create owner-only from the start, then write (a pre-existing file keeps
    # its mode, so chmod to be sure).
    payload = json.dumps(
        {"url": url, "token": token, "pid": pid if pid is not None else os.getpid()},
        indent=1,
        sort_keys=True,
    )
    path.write_text(payload)
    os.chmod(path, 0o600)
    return token
