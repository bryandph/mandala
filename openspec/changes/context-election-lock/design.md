## Context and Sources

The current contract is in `openspec/specs/fleet-context/spec.md`: one context per canonical flake, endpoint-based failover and discovery/authorization. `openspec/specs/fleet-state-formats/spec.md` pins discovery bytes and reader judgments. This change replaces bind-only arbitration, not the execution/proxy, audit, run-registry or mutation-retry contracts.

`crates/mandala-context/src/identity.rs` canonicalizes the flake and derives the key and deterministic port sequence. `lib.rs` probes discovery, scans all ports, then walks again to bind; `discovery.rs` publishes metadata atomically. These are not one atomic election. A contender can finish a stale scan, claim a newly freed earlier port and coexist with another contender already serving on a later port. Both the controlled disappearing-occupant witness and abrupt-promotion stress produced two distinct authenticated, wire-serving endpoints; retained Role flags alone are not the evidence.

`host.rs` places the listener in an accept task and Drop requests asynchronous task abortion. Its independently spawned dispatch futures are not currently cancelled by host Drop. `fleet.rs` serializes reacquisition within one session, not across independent sessions, and snapshots local dispatch outside the role mutex. `client.rs` detects TCP EOF/call failure. Those ownership boundaries must be reviewed before implementing the guard.

## Goals and Non-goals

Guarantee at most one cooperating context leader per canonical flake for a user on a host, across processes and sessions, throughout leadership. Preserve discovery, authorization, endpoint liveness, detached-run recovery and retry discipline. Support Linux and macOS, including concurrent fork paths.

Non-goals are implementation, rollout, changing port allocation or protocol bytes, mixed-version safety, arbitrary external-effect rollback, cancellation of detached supervisors, and weakening the busy-leader rule to accommodate broken loopback environments.

## Decisions, Alternatives and Rationale

### 1. A leader-lifetime process lock, not an election-only lock

Use a whole-file exclusive POSIX record lock via nonblocking `fcntl(F_SETLK)` (`F_WRLCK`, offset zero, length zero) on a dedicated descriptor. These are traditional process-associated locks, not open-file-description locks: Linux `F_OFD_SETLK` / `F_OFD_SETLKW`, `flock` and a generic file-lock wrapper with those semantics are not substitutes. The selected primitive works on Linux and macOS and is not inherited by forked children. A child blocked before exec, a failed exec, or an unrelated concurrent fork must not retain its dead parent's leadership. Set close-on-exec atomically as defense in depth; CLOEXEC alone does not solve fork-before-exec retention.

Traditional record locks do not exclude sessions in the same process. A process-wide registry supplies one keyed async mutex for each coordination identity (validated coordination root plus canonical-flake key). The registry must not split a live key into two mutexes through removal/recreation or alternate root spellings. Hold both the mutex guard and OS lock for the entire leader lifetime. Acquire the keyed mutex before opening the lock inode: closing ANY descriptor for that inode in the process can release its traditional record locks. All same-process access to an owned inode must go through the one descriptor-owning helper; no diagnostic open/close, duplicate-descriptor lifecycle or independent unlock path while held.

The guard covers the leader config factory, config publication, local dispatch and wire serving. The factory never runs for an unguarded candidate or follower. Do not retain a callable leader config beyond its guard. A host with a hung factory still owns the lock; another contender must fail safely, not proceed unlocked.

**Alternatives rejected:** election-only serialization stops the demonstrated interleaving but still trusts probes after releasing its guard; ambiguous shutdown/IO observations are not a lifetime proof. Bind-only serialization does not cover different walked ports. OFD/flock lifetime locks can survive parent death through a pre-exec child. An in-process mutex alone does not exclude other processes.

### 2. Stable, private coordination state

Use the account's stable per-user home state path `~/.local/state/mandala/context-election/`, with filenames `<identity-key>.lock` derived by the existing canonical-flake identity. It is host-local coordination, independent of `MANDALA_FLEET_STATE`, `XDG_STATE_HOME` and `XDG_RUNTIME_DIR`; do not select a different namespace from per-client overrides or absence of a runtime directory. Production home resolution must identify the same account home across launch paths rather than let a client-specific state setting choose it. An explicit acquisition option injects an isolated coordination root for tests, propagated through reacquisition and every frontend seam. It is not an automatic fallback.

When the DEFAULT coordination root is missing, acquisition creates it securely at mode 0700, including any missing parents beneath the stable account home (`.local/state/mandala`) at mode 0700, without following symlinks. Document this acquisition-owned creation behavior before implementation ships. An explicitly injected test root is never auto-created: missing means fail closed. Use descriptor-relative/no-follow operations and post-open checks to verify the root is a real directory, owned by the current user, at mode 0700, with no symlink in the path; securely validate existing parents and verify newly created parents as well. Any creation failure, unwritable root or unsafe existing path fails closed, with no alternate-root fallback. Create a missing per-identity regular lock file securely at mode 0600 in the validated root; validate existing permissions, ownership and type. Reject symlinks and foreign ownership in coordination-path resolution and opening, using post-open identity checks to avoid check/open replacement races. Do not repair an untrusted path by following it, replacing it or changing its owner.

Lock inodes are persistent rendezvous objects, not stale-lock markers. Never unlink, rename over or recreate one at release, crash recovery, discovery cleanup or token rotation. Contents are not an authority for ownership, PID, liveness or credentials. Support requires a host-local filesystem with working POSIX record locking; unsupported or uncertain locking is an explicit error, not an inferred permission to lead. Network-home locking is not qualified by this change and must not be advertised as supported without separate qualification.

**Alternatives rejected:** state-directory-relative locks split leadership across client overrides; runtime/tmp roots disappear or differ across frontends and macOS often lacks `XDG_RUNTIME_DIR`. Unlinking a supposedly stale lock permits concurrent owners on different inodes. Discovery's write-then-rename pattern is correct for metadata but incorrect for the lock inode.

### 3. Acquisition, contention and liveness

A follower may join an authenticated existing endpoint without owning a leadership guard. If joining fails and promotion is needed, bounded acquisition obtains the keyed mutex and process lock before any claim. Re-read discovery/token state and repeat the full existing-leader scan under the guard: observations made before waiting are stale. Only then perform the deterministic bind walk, construct the leader config, start serving and publish discovery. If an existing leader is found, drop the unused candidate guard and return a follower rather than constructing another host.

While another owner holds either guard, candidates join/rejoin the existing authenticated endpoint or retry coordination within one bounded acquisition budget. They may re-read discovery and rescan for joining, but may not bind, publish discovery, invoke the factory or execute a leader config without the guard. If the owner is still unavailable at the deadline, return a dedicated structured coordination busy/unavailable error. Default-root creation failure, missing injected root, unwritable root, unsafe path, unsupported locking and other lock errors are explicit failures. Never reinterpret such errors as leader death, elect unlocked, select a runtime/tmp root or turn a leader config into an unguarded standalone context.

Keep the existing busy-leader rule: a handshake timeout is retried on that endpoint within its bounded window, never skipped to claim elsewhere. Lock waits and existing probe/token retries share a total budget compatible with client initialization, rather than stacking unbounded waits. Read-only standalone behavior already allowed by other contracts is not a second shared leader and must not bypass coordination to invoke a leader factory. Followers detect death by TCP EOF, failed calls or ended subscriptions; they do not poll lock files or PIDs. A held lock says another owner exists, not that it is responsive.

Keep the deterministic port sequence and collision handling; actual bound address remains discovery's address of record. Keep bearer hello before service, canonical-flake checks, first-claim token minting, restart reuse and rotation cutoff (old authenticated connections survive; new connections need the fresh token). Rotation and discovery rewrites occur only while leadership remains guarded. Different metadata roots can prevent token sharing/joining; that is safe unavailability, never a reason to create another leader.

### 4. Teardown is an ownership order, not a notification order

One lease owns the listener and identity guard so the listener actually closes before the lock is unlocked or its descriptor closes. Graceful shutdown retains its bounded drain, closes connections/listener, performs guarded discovery cleanup if applicable, then releases the leadership guard. Drop and acquisition cancellation, bind exhaustion, factory failure/panic, host-start failure and discovery-publication failure use the same order. No detached accept task may retain a listener after its caller releases the lock. Aborting a task is only a request; moving the guard into the listener-owning lease or another proven ownership arrangement must make real teardown ordering hold even when Drop cannot await.

Prevent local dispatch snapshots or accepted wire work from escaping the guard's lifetime. The precise dispatch teardown is conditional on the investigation below. A supervised detached deploy remains a separate run owner discoverable through the registry, not a context leader, and is not cancelled just to release the context lock. Guard release must not delete its rendezvous inode. Process death releases the process lock without PID cleanup, including when children remain alive.

### 5. Upgrade precondition, not a mixed-version guarantee

Old binaries never take this lock. The guarantee applies only after every context frontend for the affected user/host has been stopped, upgraded across all launch paths and restarted together. This includes TUI, MCP and any CLI context host/candidate. Merely upgrading a winner, checking discovery, deleting files or rotating tokens cannot fence an older contender. Retain existing discovery/wire bytes; do not claim that protocol compatibility is election safety. The actual coordinated rollout requires a separate operator decision. Until shipped, use one TUI or MCP context frontend per checkout per host.

### 6. Regression evidence and environmental qualification

Preserve the diagnostic patch as pre-fix evidence on the implementation branch, not this documentation branch. Rework the controlled witness: the slow contender owns the guard while gated; an explicit test hook proves the fast contender attempted acquisition but remains a join/wait candidate. Release the gate only after that observation, then prove exactly one authenticated endpoint serves the flake and every successful participant converges. An unchanged witness deadlocking/timing out under serialization is not a passing regression.

Test separate processes, independent same-process sessions and independent flake identities. Use actual wire calls and dispatch labels, not only `is_leader()`. Cover SIGKILL, orderly drain, Drop and every acquisition failure, stale discovery, changing collision occupants, token rotation and guard-protected local dispatch. A fork child must be intentionally held before exec while the parent dies; replacement leadership must succeed with the child still alive. Include failed exec and unrelated concurrent forks, not just successful prompt exec.

Test different client state/runtime roots with the same coordination root, missing `XDG_RUNTIME_DIR`, secure creation of the missing default root and parents, missing injected root, root creation failure, unwritable root, unsupported locking, symlinks, foreign ownership and persistent inode identity. A foreign-owner fixture must not require changing live user files; use an isolated privileged fixture where available or injected validation metadata with a documented native-test limitation.

Before context tests, verify bounded connects to reserved-for-test but currently unused `127.0.0.1` ports return connection-refused, not timeout. Fail with a clear environment diagnostic when this assumption fails; do not weaken production busy classification. On WSL, build the test binaries first, then run an immutable copy in a private namespace, for example:

```sh
unshare --user --map-root-user --net sh -c '
  ip link set lo up
  exec "$1" --test-threads=32
' sh /absolute/path/to/copied-context-test-binary
```

Provision isolated scratch state/runtime and injected coordination roots for these tests. Repeat context/failover suites under controlled owned CPU load, with cleanup, at `--test-threads=32` in the private namespace. A private namespace is not proof of a full Nix sandbox build. Require sandboxed `nix build .#mandala-rs` and native Linux AND macOS lock/lifecycle/fork runs before qualification; macOS uses its native loopback environment with the same preflight, not Linux netns commands. No CI or flake edits are required by this plan.

## Scoped Open Question and Implementation Gate

Determine whether `RunningHost` can be dropped while its process continues: demotion, in-process restart, explicit shutdown/drop and frontend ownership paths must be traced, including local dispatch snapshots. The independently spawned call futures make socket closure alone insufficient evidence that work stopped.

- If a live-process host drop is reachable, track dispatched calls with a JoinSet or abort handles (including equivalent local-call ownership) and abort them on drop before the lock is released. Prove cancellation/termination ordering, not merely issuance of an abort request. Graceful drain remains bounded before unfinished calls are aborted. This is not rollback of already-performed external effects or cancellation of detached supervisors.
- If no production lifecycle permits it, document that the Drop-based diagnostic models connection loss only, not process death or cancellation of in-process effects. Make tests reflect that scope and use actual process termination for crash guarantees. Record the evidence supporting that conclusion rather than assuming it from the Role enum.

This investigation is the first implementation task. The selection of its conditional branch is deliberately unresolved here; do not broaden it into general dispatch redesign without review. The lifetime-lock, root, fork and fail-closed decisions above are settled. Artifact completeness does not authorize implementation or live rollout.
