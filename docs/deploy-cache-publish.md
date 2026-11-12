# Publishing built profiles before deployment

The native deploy engine can upload its verified profile closures to an existing
Nix cache once, before any host copies begin. This helps destination substitution
find paths built locally on the controller. Publishing is an optimization, not a
new deployment or trust mechanism.

## Configuration and overrides

Set `MANDALA_CACHE_URL` to a writable Nix store URL, for example
`https://cache.acme.example`. If the store needs netrc authentication, set
`MANDALA_CACHE_NETRC_FILE` to an existing netrc file's path. The engine does not
open that file or acquire credentials; Nix handles authentication. A consumer's
controller wrapper may supply this environment, but credential acquisition is
outside Mandala.

A nonempty URL enables publishing automatically; netrc is optional. Netrc alone
does not enable it. Empty environment values count as unset. Nonempty values
are not trimmed into different paths or URLs.

- `--cache-push` explicitly enables publishing and requires a nonempty URL.
- `--no-cache-push` disables it even when configured, ignoring upload settings.
- Both flags together are an argument error, regardless of order.
- An enabled malformed URL is a preflight error, before run registration/build.
  Backend support and upload/authentication failures are decided by Nix later.

Both `mandala deploy run` and `mandala tui deploy` accept these flags. The
explorer deploy confirmation cycles cache publishing through auto/on/off with
`c`; each new confirmation defaults to auto. MCP `deploy` accepts optional
`cache_push: true` or `false`; omitting it preserves auto detection in the engine
environment. Existing confirmation gates still apply.

## Ordering and outcomes

The engine resolves the selection, batch-builds once and, when necessary,
recovers only exact successful outputs with building and substitution disabled.
It then sorts/deduplicates the successful profile paths and invokes one command:

```text
nix copy --to <configured-store-url> [--option netrc-file <netrc-path>] <unique-profile-path>...
```

Arguments are passed directly, not interpolated into a shell. Nix transfers
transitive closures and skips paths already present. The engine neither builds
again nor uploads per host. Profile roots remain alive through upload and
fan-out. Failed/unverified outputs are never uploaded; no successful paths means
no upload. Resolution errors, interrupted builds and halt-on-build-failure aborts
never enter the phase.

Only after publication settles does per-host fan-out start. Ordinary upload
spawn/exit failure produces a warning, a safe reason and process status when
available; it does not retry automatically or fail the run by itself. Each
eligible host still executes its existing SSH copy, whether publication succeeded
or failed. Destination substitution is already enabled unless the flattened
host setting `fastConnection` is true; publication does not override it. Missing
paths still travel over SSH. A successful upload is not proof that a host can
reach or trust the cache. Build failures, host failures and rollback keep their
existing result precedence.

`--dry-activate` still builds, publishes and copies, but performs no live or boot
activation. Run-level `--boot` and per-member boot settings use the same publish
phase and preserve successful reboot-pending outcomes. The internal Nix dry-run
resolution probe does not upload; no general `--dry-run` option is added. Static
`deployBatch.<group>` cache-warming remains unchanged.

## Credentials and trust

Only the upload Nix child receives the netrc path, through its explicit Nix
option. The supervisor may pass the upload environment to the engine, but
ordinary build, recovery, evaluation and SSH subprocesses do not inherit
`MANDALA_CACHE_NETRC_FILE`. Metadata and recorded launch argv do not contain the
netrc path.

Executable upload arguments stay separate from diagnostic data. Recorded URLs
omit userinfo, query and fragment. Raw upload stdout/stderr is discarded rather
than logged: a credential-agnostic engine cannot safely scrub credentials it
never reads. Errors use classified reasons such as `spawn-failed` or `copy-exit`
plus an exit code when available; they do not echo credential paths or raw
backend errors. Detailed authentication debugging must be done separately, not
by enabling raw output in durable deploy logs. The process's actual argv is
still subject to the local operating system's process-inspection permissions.

The cache must re-sign with a key the hosts already trust, and hosts must already
have it configured as a substituter. Mandala adds no signing keys, changes no
host substituters, signs no paths and adds no upload signature bypass. No cache
settings or credentials enter the fleet aggregate. Builder publishing, cache
garbage collection and per-host cache selection are outside this feature.

## Durable observation and termination

`meta.json` has an additive `publish` object: `enabled`, optional redacted
`cache_url`, `outcome`, `path_count`, optional `reason` and optional `process_rc`.
Outcomes distinguish pending/running/succeeded/failed/skipped/interrupted.
Skipped reasons distinguish disabled, no-built-paths and build-not-completed.
A shared `publish.jsonl` stream uses the existing event version, plugin
`publish`, empty host tag and `actor: controller`. Its `milestone: publish`
records carry the same safe outcome data. Eligible hosts receive that shared
outcome as a non-terminal publish milestone before copy, not as a failed host
status. An upload is never counted as another targeted host.

Headless output, owned and attached TUI views, the runs list and MCP
`deploy_status` show the active cache-publish phase and/or its safe outcome.
The TUI retains warnings in the build header and summary; MCP includes the safe
object in curated metadata, without requiring diagnostics mode. Older runs
without publish data remain readable; older readers ignore the additional data.

Publication follows [native process ownership](deploy-termination.md).
SIGINT/SIGTERM is not an advisory cache failure: termination forwards signals to
the owned group, escalates after the existing two-second grace and prevents new
publish, recovery or fallback SSH effects. The run fails. A reaped group's ID is
never signalled later on drop. Detaching a screen still does not terminate a run.
