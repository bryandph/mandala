## MODIFIED Requirements

### Requirement: One shared execution context per checkout
The system SHALL maintain at most one fleet execution context per canonical flake path for a user on a host: a single leader process owns tool execution, the warm evaluator, the activity stream and audit log, and serves a loopback coordination endpoint. Leadership SHALL require a per-canonical-flake exclusive process-associated POSIX record lock AND a keyed in-process async mutex held for the leader's whole lifetime, not just election. The endpoint bind SHALL select a service address, not arbitrate leadership across walked ports. No leader config SHALL be constructed by its factory, published or invoked without holding both guards. Any long-lived mandala frontend (MCP server or TUI) SHALL be able to host the context; different canonical paths SHALL have independent contexts. This guarantee SHALL be subject to the coordinated-upgrade precondition below.

#### Scenario: a second instance joins instead of duplicating
- **WHEN** a process starts against a checkout with a live context leader
- **THEN** it joins as follower or observer, or reports safe bounded unavailability, without creating another execution context, evaluator or activity stream

#### Scenario: worktrees are isolated
- **WHEN** processes use two different canonical checkout paths
- **THEN** each path has its own coordination identity and independent leader, and neither is served results from the other's checkout

#### Scenario: a disappearing collision owner invalidates a scan
- **WHEN** an earlier foreign port occupant exits after one contender scanned it and another contender attempts election for the same flake
- **THEN** the lifetime guard prevents two leaders on different walked ports, regardless of the obsolete scan result

#### Scenario: independent sessions in one process contend
- **WHEN** independent sessions in one process acquire the same canonical flake and coordination root concurrently
- **THEN** the keyed async mutex permits only one leader lease and the other sessions join or fail safely without invoking their leader factories

### Requirement: Leader failover without losing running work
When a leader dies, surviving participants SHALL detect TCP EOF, call failure or subscription closure and attempt bounded guarded reacquisition. Exactly one eligible participant SHALL obtain the lifetime guards before promoting; others SHALL reconnect or return safe busy/unavailable outcomes. Followers SHALL NOT detect death by polling the lock or recorded PID. A registered run whose owning process died SHALL remain discoverable and attachable through the shared run registry. An in-flight forwarded call SHALL surface a structured failover error rather than hang; idempotent reads MAY be retried once after promotion, and mutating calls MUST NOT be retried automatically. A detached deploy SHALL remain owned by an independent supervisor that keeps event and diagnostic streams writable, reaps the child and atomically records terminal metadata when the launcher dies.

#### Scenario: a deploy survives its leader process
- **WHEN** the leader is forcibly terminated while its detached deploy is running
- **THEN** the deploy and supervisor continue with writable streams and terminal metadata, and the replacement leader attaches to the same registered run

#### Scenario: a promotion race has one winner
- **WHEN** followers in distinct processes detect leader death and promote concurrently
- **THEN** the process lock arbitrates one leader, all successful joiners follow its authenticated endpoint, and no other contender publishes or executes a leader config

#### Scenario: a supervisor dies before settlement
- **WHEN** both launcher and detached supervisor disappear before terminal metadata is recorded
- **THEN** status inspection reports explicit orphaned failure instead of a permanently running deploy

#### Scenario: failure leaves a mutation outcome unknown
- **WHEN** a forwarded mutation loses its leader connection before receiving its result
- **THEN** the caller receives structured failover without automatic replay, regardless of which participant next owns the lock

### Requirement: Context discovery and authorization
Each context SHALL publish a discovery file in the mandala state directory, keyed by canonical checkout, recording endpoint address, bearer token, leader PID and flake path at mode 0600. Discovery SHALL remain metadata, not the election lock. Connections SHALL authenticate with the bearer token before other service. Liveness SHALL be judged through the endpoint; PIDs SHALL remain advisory. The deterministic port walk and discovery's actual-bound-address authority SHALL remain unchanged. A candidate that obtains the lifetime guards SHALL re-read discovery/token state and rescan for an existing leader before binding, invoking its factory or publishing a claim.

A handshake timeout SHALL remain a busy-leader observation, retried on the same endpoint within a bounded window, never permission to skip it and bind another leader. Only refused connections or definitive protocol answers permit advancement under the existing walk rules, and no such observation overrides a held identity lock. Acquisition waits, probe retries and token refresh SHALL fit a bounded total client-initialization budget. Exhaustion SHALL fail acquisition loudly without binding a second endpoint or rewriting discovery. No fallback SHALL invoke an unguarded leader config. Existing standalone read-only evaluation, where otherwise permitted, SHALL NOT constitute a second shared leader or bypass these guards.

Token minting at first claim, reuse across restart and rotation SHALL remain unchanged: rotation rejects the old token on new connections while already-authenticated connections live out their lives. Discovery publication and rotation SHALL occur only under the lifetime guards.

#### Scenario: stale discovery does not block a claim
- **WHEN** discovery points to a refused endpoint with a possibly recycled PID and no owner holds the lifetime guards
- **THEN** a guarded candidate re-reads discovery, rescans, binds and rewrites discovery without PID-based cleanup

#### Scenario: an unauthorized connection is rejected
- **WHEN** a local client lacks the discovery bearer token
- **THEN** the endpoint serves nothing beyond authentication failure

#### Scenario: a busy leader is not usurped
- **WHEN** a live leader cannot finish the handshake within the probe timeout
- **THEN** the joiner retries that endpoint within its bounded window and joins or fails safely, never binding a new leader endpoint or rewriting discovery

#### Scenario: acquisition stays inside the client handshake budget
- **WHEN** a frontend encounters lock contention and a busy or unresponsive endpoint
- **THEN** the combined waits and retries complete or fail in time for client protocol initialization rather than accumulating separate unbounded windows

#### Scenario: rotation races a joiner
- **WHEN** a joiner presents an old token during rotation
- **THEN** it refreshes discovery and converges on the fresh token within the existing bound while already-authenticated connections remain valid and no second leader is created

## ADDED Requirements

### Requirement: Process locks exclude contenders without fork inheritance
The OS guard SHALL be a whole-file exclusive traditional POSIX `fcntl(F_SETLK)` record lock (`F_WRLCK`, start zero, length zero), associated with the process, paired with a process-wide keyed async mutex. It SHALL NOT use `F_OFD_SETLK`, `F_OFD_SETLKW`, `flock` or equivalent open-file-description semantics. Fork children SHALL NOT inherit ownership of the leadership lock or delay its release after parent death, even before exec or after exec failure. Close-on-exec SHALL be set atomically but SHALL NOT be considered sufficient for this lock-ownership property. A dedicated descriptor-owning helper SHALL serialize same-process access before opening the inode and prevent other opens/closes of that inode from silently releasing the process's lock. The keyed mutex registry SHALL NOT create parallel guards for one live identity.

The fork guarantee covers the lock, not immediate closure of inherited context socket copies. A pre-exec child retaining the listener or accepted connections MAY delay socket takeover and existing followers' EOF/in-flight completion until the child execs or exits. Socket-level failover latency SHALL NOT be claimed as bounded during that window. Acquisition SHALL fail closed on an inherited busy listener: no leader factory, discovery rewrite, second authenticated endpoint or unlocked execution. After inherited sockets are released, participants SHALL converge on exactly one authenticated replacement endpoint. This documented inherited-copy limitation also applies when interpreting listener-closure and bounded-failover obligations; the owner still closes its own listener and work in the required lease order.

Production code SHALL NOT fork without an immediate exec. Workspace-owned child creation while context resources may be live SHALL use `std::process::Command` or `tokio::process::Command`; application `pre_exec` hooks SHALL be limited to non-blocking descriptor-flag changes. This rule does not claim to audit internals of already-execed external tools. No atfork socket registry or heartbeat-deadline redesign is required by this change. The stronger parked-child serving witness SHALL remain ignored as documented evidence of the socket-retention limitation, not be reported as a passing guarantee.

#### Scenario: separate processes cannot hold the same lease
- **WHEN** distinct processes request the same coordination identity
- **THEN** at most one holds the OS lease and the loser cannot invoke its factory, publish discovery or serve as leader

#### Scenario: parent dies with a child blocked before exec
- **WHEN** a leader forks a child that remains blocked before exec and the leader is killed
- **THEN** another process can acquire the leadership lock while the child is still alive, without child-side unlock of the parent's lock; inherited socket copies may still delay actual endpoint takeover until exec or exit

#### Scenario: unrelated concurrent fork or failed exec
- **WHEN** another runtime thread forks or a child fails exec while leadership is held
- **THEN** the parent retains exclusion while alive and children cannot retain the lock after parent death, including when a failed-exec child remains alive

#### Scenario: a paused child retains the listener
- **WHEN** the parent dies while a child is held before exec with an inherited listener
- **THEN** replacement acquisition returns LeaderBusy without invoking its factory, rewriting discovery, executing unlocked configuration or creating a second authenticated endpoint

#### Scenario: a paused child retains an accepted connection
- **WHEN** a pre-exec child retains an accepted context connection after parent death
- **THEN** an existing follower's EOF and in-flight completion may be delayed until child exec or exit, and bounded socket-level failover is not promised during that window

#### Scenario: the inherited socket window ends
- **WHEN** the child execs or exits and releases its inherited context sockets
- **THEN** replacement succeeds with exactly one authenticated serving endpoint and all successful participants converge; a successfully execed child may remain alive without retaining the sockets

#### Scenario: production launch paths obey immediate exec
- **WHEN** production context-capable code starts a child, including a detached deploy supervisor
- **THEN** it uses Command or tokio Command with immediate exec, without a fork-only continuation or deliberately blocking pre_exec hook; application hooks are limited to non-blocking descriptor-flag changes

### Requirement: Listener closure precedes leadership release
The leader lease SHALL keep the listener and lifetime guards in one proven ownership arrangement. On graceful shutdown, Drop, cancelled acquisition and every failure/unwind path, the listener SHALL actually close before either guard is released. An accept-task abort request alone SHALL NOT prove closure. No leader config, local dispatch snapshot or wire dispatch SHALL execute beyond its guard's lifetime. Graceful shutdown SHALL retain bounded draining and guarded discovery cleanup; stale discovery after abrupt loss SHALL remain recoverable without unlinking the lock inode.

Before implementation, lifecycle investigation SHALL establish whether a host can be dropped while its process lives, including demotion and in-process restart. If reachable, dispatched calls SHALL be tracked with a JoinSet or abort handles (and equivalent ownership for local calls), aborted on Drop and stopped before lock release. If unreachable in production, the lifecycle evidence SHALL be documented and Drop-based tests SHALL describe connection-loss semantics only, not claim process death or cancellation of continuing effects. Actual crash guarantees SHALL be tested with process termination. Independent detached supervisors SHALL NOT be treated as context dispatch to cancel.

#### Scenario: graceful drain retains exclusion
- **WHEN** a leader drains in-flight calls during orderly shutdown
- **THEN** it holds both guards until its listener is closed and teardown obligations are complete, after which a contender may promote

#### Scenario: Drop cannot release ahead of an accept task
- **WHEN** a host is dropped and its accept task has not yet processed cancellation
- **THEN** the guard stays owned until actual listener closure, preventing a new leader from overlapping the old listening endpoint

#### Scenario: acquisition fails after binding
- **WHEN** factory construction, host startup, discovery publication or acquisition cancellation fails after a listener was bound
- **THEN** cleanup closes that listener before releasing the guards and leaves no detached serving leader

#### Scenario: a live-process drop is reachable
- **WHEN** lifecycle investigation identifies a reachable host drop while dispatched calls can still run in the process
- **THEN** implementation and tests require tracked-call abortion and termination before release, covering local and remote calls rather than socket closure alone

#### Scenario: Drop is only a connection-loss model
- **WHEN** lifecycle investigation proves production host drop cannot occur independently of process exit
- **THEN** the conclusion and evidence are documented, diagnostic Drop tests make only connection-loss claims, and process-kill tests establish crash behavior

### Requirement: Coordination failures never create an unlocked leader
When another owner holds either lifetime guard, a candidate SHALL join the existing authenticated leader or retry within a bound, then return a dedicated structured coordination busy/unavailable error if still unsuccessful. Acquisition SHALL securely create a missing DEFAULT coordination root and its missing parents beneath the account home at mode 0700 without following symlinks, then verify directory type, current-user ownership, root mode 0700 and path safety after opening, as specified in `fleet-state-formats`. An explicitly injected test root SHALL NOT be auto-created. Default-root creation failure, missing injected root, unwritable root, unsafe paths, unsupported locking and all other coordination errors SHALL fail closed with explicit diagnostics. No path SHALL elect unlocked, silently move coordination to runtime/tmp state, publish discovery or invoke a leader config without both guards. A held but unreachable owner SHALL remain excluded from replacement; responsiveness SHALL NOT be inferred from lock contents.

#### Scenario: owner holds the lock before discovery publication
- **WHEN** a contender cannot join because the lock owner has not yet published or finished its factory
- **THEN** it waits/retries within the bound and joins or returns coordination busy/unavailable, without binding or invoking its own factory

#### Scenario: coordination is unavailable
- **WHEN** default-root creation fails, an injected root is missing, the root is unwritable, a path is unsafe or record locking is unsupported
- **THEN** acquisition returns a classified explicit error with no unlocked election, alternate-root fallback or leader-side execution

#### Scenario: different metadata roots cannot bypass exclusion
- **WHEN** two clients use the same flake and coordination root but different state/runtime environment roots and cannot share a usable token
- **THEN** only one may lead and the other joins if authorized or returns bounded safe unavailability, never creates a second leader

### Requirement: Election safety requires coordinated upgrades
The lifetime-lock guarantee SHALL require all context frontends for the user/host to be stopped, upgraded across every launch path and restarted together. Older binaries that do not take the lock SHALL NOT be considered fenced by discovery compatibility, token rotation or the presence of a lock file. Rollout SHALL require a separate operator decision, not follow automatically from these artifacts. Until the fix ships, the interim rule SHALL remain one TUI or MCP context frontend per checkout per host.

#### Scenario: an old frontend remains running
- **WHEN** any old TUI, MCP or other context-capable frontend remains active or can acquire concurrently
- **THEN** mixed-version safety is not claimed and the coordinated-upgrade precondition remains unmet

#### Scenario: all launch paths are upgraded together
- **WHEN** all context frontends for the user/host have been stopped, upgraded and restarted with participating binaries
- **THEN** they share the new lifetime-lock contract while retaining the discovery and authorization protocol

### Requirement: Context tests verify loopback refusal before election
Context tests SHALL preflight bounded connects to unused test `127.0.0.1` ports and fail with a clear environment diagnostic unless connections are refused rather than timed out. This test-only preflight SHALL NOT weaken production busy-leader behavior. Test documentation SHALL describe WSL execution in a private network namespace using `unshare --user --map-root-user --net` with loopback brought up, isolated roots and prebuilt immutable test binaries. Qualification SHALL require Linux and macOS runs, repeated context/failover suites at `--test-threads=32` under controlled CPU load in a private Linux namespace, and a sandboxed `nix build .#mandala-rs`; native macOS runs SHALL use the same refusal preflight without requiring Linux namespaces.

#### Scenario: unused loopback ports time out
- **WHEN** an environment blackholes connects to unused loopback test ports
- **THEN** tests stop with an actionable refusal-preflight failure instead of presenting initial acquisition busy errors as election failures or skipping a busy leader

#### Scenario: private loopback restores test assumptions
- **WHEN** prebuilt tests run in a private Linux network namespace with loopback up and the preflight passes
- **THEN** election assertions run under the required refusal semantics, with stress evidence reported separately from full sandbox-build evidence
