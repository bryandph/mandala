# MCP golden fixtures — the parity oracle

These JSON files are the **result-shape contract** for the mandala MCP tool
surface. They are captured from the current FastMCP Python server and are the
oracle the Rust port asserts against (OpenSpec change `mandala-rust-rewrite`,
section 4 — `fleet-mcp` parity).

Each file is one tool invocation's `result.data`, for all 12 tools across the
ok / refusal / error paths:

| tool | fixtures |
|------|----------|
| `members` | `members.compact`, `members.full` |
| `groups` | `groups.ok` |
| `resolve` | `resolve.ok` |
| `ping` | `ping.mixed` (SUCCESS + UNREACHABLE, stderr→`diagnostics`) |
| `host_eval` | `host_eval.ok`, `host_eval.eval_error` |
| `drift` | `drift.ok`, `drift.filtered`, `drift.eval_error` |
| `reload` | `reload.ok`, `reload.unavailable_error` |
| `deploy_status` | `deploy_status.command`, `deploy_status.deploy`, `deploy_status.list` |
| `build` | `build.ok` |
| `deploy` | `deploy.refused`, `deploy.dry_ok` |
| `restart_service` | `restart_service.refused`, `restart_service.partial` |
| `reboot` | `reboot.refused`, `reboot.ok` |

## How they were captured

Deterministically, through FastMCP's in-memory `Client` over an **injected
aggregate** (no `nix eval`), with subprocess/launch points monkeypatched — the
same headless path `cli/tests/test_mcp.py` uses. No real fleet, ansible, or nix
is required. See `capture_fixtures.py`.

Regenerate (from `flakes/mandala/`, in a python env with `fastmcp`):

```
PYTHONPATH=cli/src python cli/tests/fixtures/mcp/capture_fixtures.py
```

## Volatile fields

Parity tests must assert on **keys and non-volatile values**, never on these
(the capture normalizes them to placeholders so the fixtures don't churn):
`run_id`, `events_dir`, `log`, `meta.pid`, `elapsed`, `ts`, and any
`/nix/store` or state-dir path.
