# State and collection hygiene

Deploy coordination logs live in `<state>/deploy-launches/`, separate from
registered `runs/`. Allocating a launch prunes inactive launches with the
same policy as runs: keep the newest 20 and expire those at least 30 days
old. Both use `MANDALA_FLEET_RUN_KEEP` (minimum 1) and
`MANDALA_FLEET_RUN_MAX_AGE_DAYS` (0 disables age expiry).

The allocating frontend PID protects unpublished launches. A shared file
lock is inherited by the supervisor/engine across exec, protecting even a
frontend death between spawn and PID publication. Pruning requires an
exclusive nonblocking lock. An atomic `active.json` also records the
supervisor or inline engine PID after spawn.
Live supervisor/engine PIDs, including engines found through the log's
published run ID, are never pruned and do not count against the inactive
budget. Unknown directory names, symlinks and unreadable activity metadata
are left alone. PID reuse conservatively retains extra state, as for runs.
Pruning is best-effort and never touches directories outside this state
root. Logs from a live run are intentionally not size-capped by this policy.

The Python deploy/action plugins were removed in the Rust transition.
A fresh checkout has no `__pycache__` or bytecode from them. Git ignores
Python cache artifacts, the Rust package's Nix source fileset includes only
Cargo manifests and `crates/` (not `ansible/`), and Galaxy packaging excludes
`__pycache__`, `*.pyc`, `*.pyo` and `*.pyd`. Python does not import orphan
PEP 3147 cache files without their source modules.

For an old checkout, inspect and remove only caches under that checkout:

```sh
# Run from the mandala repository root; preview before deleting.
find ./ansible -type d -name __pycache__ -print
find ./ansible -type f \( -name '*.pyc' -o -name '*.pyo' -o -name '*.pyd' \) -print
# After reviewing the preview:
find ./ansible -type f \( -name '*.pyc' -o -name '*.pyo' -o -name '*.pyd' \) -delete
find ./ansible -depth -type d -name __pycache__ -empty -delete
```

Do not use a home-directory-wide cleanup or remove
`~/.local/state/mandala`. No real state cleanup is part of this change.

ForestWidget's unused optional ratatui feature is deferred to LAB-28,
where wiring or removal can be decided alongside native build output.
