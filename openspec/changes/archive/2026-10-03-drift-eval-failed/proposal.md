No public tracking issue.

## Why

Expected-toplevel evaluation was all-or-nothing. A single member whose
NixOS configuration failed to evaluate aborted the whole batch, so one bad
host hid the drift judgement of every other host. Reporting that failure
against the member alone is not enough on its own: drift then judges the
failed host with no expectation, which falls through to `in-sync` or
`activated`. That is the green-status-on-missing-data outcome the drift
specs already reject for stale, incomplete, never-surveyed, and unreachable
hosts.

## What Changes

- Expected-toplevel evaluation isolates failures per member. A failure
  shared by the whole flake (locking it, forcing its outputs or
  `nixosConfigurations`) still fails the call; a failure in one member's
  configuration is reported against that member while the others evaluate.
  Both backends do this: the worker per navigation, the subprocess backend
  by re-evaluating members one at a time after a failed batch.
- Members with no `nixosConfigurations` entry are skipped by both backends
  (previously the subprocess backend failed the whole eval).
- A new drift status, `eval-failed`, replaces any judgement that needs the
  expectation (in-sync, drift, reboot-pending, activated) for a host whose
  expectation failed. Survey-side statuses (no-snapshot, unreachable,
  incomplete, stale) still take precedence. Drift entries gain an optional
  `eval_error` field, omitted when there is no error.
- A partial evaluation is never written to the rev-keyed expectation cache.
- CLI `drift --eval` still renders every host, lists per-host failures on
  stderr, and exits non-zero. The MCP `drift` tool reports `ok: false` and
  an `eval_failed` host list; `host_eval toplevel=true` reports a member's
  own failure as `eval_error`. The TUI shows the failed hosts as
  `eval-failed` and a sticky status naming them.
- The worker wire value for `expected_toplevels` maps each member to its
  out-path string or an `{"error": …}` object.

## Capabilities

### Modified Capabilities

- `fleet-cli`: drift judgement adds `eval-failed`; partial evaluations are
  not cached.
- `fleet-mcp`: drift reporting carries the same status and per-host errors.

## Impact

`DriftStatus` (a pinned contract) gains a variant; consumers that match on
the status strings must handle `eval-failed`. The `.expected.json` cache
format is unchanged.
