## Why

The current context election scans candidate ports and then binds in a separate pass. A disappearing collision owner can invalidate that scan: two contenders can retain different listening ports for the same canonical flake and both execute authenticated calls. Binding excludes another listener on one address, not another leader elsewhere in the port walk. Atomic discovery publication does not serialize election.

## What Changes

- Replace the "bind is the lock" contract with a per-canonical-flake, leader-lifetime process-associated POSIX record lock and a keyed in-process async mutex.
- Coordinate through `~/.local/state/mandala/context-election/<identity-key>.lock`, independent of client state/runtime overrides, with private permissions, secure inode handling and an explicit test-root seam.
- Require actual listener closure before lock release on graceful shutdown, Drop and every error path; never publish or invoke leader configuration without the guard.
- Define bounded join/retry and explicit safe busy/unavailable outcomes when coordination fails; retain TCP-based follower death detection and the busy-leader rule.
- Preserve the deterministic port walk, discovery format, bearer authorization, token reuse and rotation. Add coordinated-upgrade preconditions because older binaries do not participate in locking.
- Specify deterministic serialization, process/fork, lifecycle, filesystem and Linux/macOS qualification; add a test-only loopback-refusal preflight and private-network-namespace guidance.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `fleet-context`: leader ownership, failover, discovery and authorization; lock lifetime, safe acquisition failures, rollout and context-test environment requirements.
- `fleet-state-formats`: persistent coordination-file layout and inode/security contract, distinct from discovery metadata and serialized run state.

## Impact

Later implementation affects `crates/mandala-context/src/{lib,host,identity,discovery,fleet,client}.rs`, an identity-lock helper and frontend acquisition-option plumbing. The local execution seam and retained dispatch ownership need lifecycle review as well as remote serving. Discovery bytes and the wire protocol need no change. The persistent lock-file namespace is an additive compatibility contract even though file contents carry no leader metadata.

This change contains planning artifacts only. No public tracking issue exists. Artifact completeness is not implementation or rollout approval.

## Non-goals

Code, tests, canonical-spec synchronization before archive, deployment, CI/flake changes, automatic migration, mixed-version fencing, stale-PID cleanup, lock polling for follower liveness, changing busy-leader classification, or automatic mutation retry. General cancellation of detached supervisor-owned work is excluded. The narrowly conditional handling of dispatched calls during an in-process host drop is a prerequisite investigation, not an assumed lifecycle fix.

Until the fix ships, the interim operating rule is one TUI or MCP context frontend per checkout per host.
