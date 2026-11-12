No public tracking issue.

## Why

The native engine builds selected profiles once, but locally built closures may not exist in the hosts' configured caches. Repeating their transfer over controller-to-host SSH wastes the controller's uplink. The existing destination substitution in `crates/mandala-deploy/src/push.rs` cannot substitute a path that was never published.

## What Changes

- Add an optional controller-side publish phase after the single batch build and successful-output recovery, before any per-host fan-out.
- Publish the deduplicated union of verified built profile paths in one `nix copy --to <cache>` invocation. Nix copies their closures and skips paths already present.
- Select publishing with `MANDALA_CACHE_URL`, optionally supply upload authentication through `MANDALA_CACHE_NETRC_FILE`, and expose explicit `--cache-push` / `--no-cache-push` overrides.
- Keep the engine credential-agnostic and preserve existing host substituters, trusted keys, copy settings, activation behavior, and result semantics.
- Warn and continue the existing per-host SSH copy on ordinary publish failure; record a non-fatal `publish` milestone and reason across registry, CLI status, TUI, and MCP observers.
- Extend effect-isolated tests for ordering, argv, fallback, override precedence, credential redaction, and process ownership.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `fleet-deploy`: optional, observable, best-effort cache publishing between verified batch build and host fan-out, with credential and termination boundaries.

The delta includes additive registry and frontend requirements for this deploy phase. It does not replace the shared format compatibility rules in `openspec/specs/fleet-state-formats/spec.md` or the frontend contracts in `openspec/specs/fleet-cli/spec.md`.

## Impact

Future implementation touches the native engine in `crates/mandala-core/src/engines/deploy.rs`, process ownership in `crates/mandala-deploy/src/process.rs`, registry projections, CLI/TUI/MCP launch and observation paths, and isolated regression tests. `crates/mandala-deploy/src/push.rs` already requests destination substitution unless `fastConnection = true`; that setting remains authoritative. Static `deployBatch.<group>` cache-warming artifacts remain unchanged.

This change contains planning artifacts only. Artifact completeness is not plan approval or implementation authorization.

## Non-goals

- A controller credential wrapper or secret-store integration in any consumer repository.
- Builder-side publishing, remote builds, cache garbage collection, or per-host cache selection.
- Adding signing keys, configuring host substituters, or changing the existing cache trust path.
- Replacing destination substitution, overriding `fastConnection`, rebuilding to warm caches, or changing activation/rollback policy.
- Implementation, live cache uploads, fleet mutation, or consumer migration before separate plan approval.
