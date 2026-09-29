# fleet-context Specification

## Purpose
One shared execution context per checkout: a leader process owns evaluation, runs and checkout watching, while followers (CLI, TUI, MCP) proxy execution to it, observe it live, and take over on leader failure without losing running work.

## Requirements
### Requirement: One shared execution context per checkout
The system SHALL maintain at most one fleet execution context per canonical
flake path: a single leader process that owns tool execution, the warm
evaluator, the activity stream, and the audit log for that checkout, and
serves a loopback coordination endpoint for other mandala processes.
Leadership SHALL be acquired by atomically binding the per-checkout
endpoint — the bind is the lock — so that leadership releases itself on
process death without any stale-lockfile protocol. Any long-lived mandala
frontend (an MCP server instance or the TUI) SHALL be able to host the
context; checkouts with different canonical paths SHALL get independent
contexts.

#### Scenario: a second instance joins instead of duplicating
- **WHEN** a mandala process starts against a checkout whose context endpoint
  is already bound by a live leader
- **THEN** it joins that context as a follower or observer rather than
  creating a second execution context, warm evaluator, or activity stream

#### Scenario: worktrees are isolated
- **WHEN** mandala processes run against two different canonical checkout
  paths (e.g. a main checkout and a workmux worktree)
- **THEN** each checkout has its own independent context and leader, and no
  process is served results evaluated from the other checkout's state

### Requirement: Followers proxy execution to the leader
A follower MCP instance SHALL serve the client-facing protocol conversation
locally (initialization and the static tool list) while forwarding tool
execution to the leader, so that results are identical whether the leader or
a follower serves the client, exactly one execution context performs the
work, and the confirm-gate and audit semantics are applied once, at the
leader, with the originating client recorded.

#### Scenario: a proxied call matches a leader-served call
- **WHEN** the same tool call is issued through a follower and directly
  through the leader
- **THEN** the results are identical in shape and semantics, and both calls
  appear in the one activity stream and (if mutating) the one audit log,
  labeled with their originating client

### Requirement: Leader failover without losing running work
When the leader dies, surviving participants SHALL re-race the endpoint
bind so that exactly one promotes to leader and the rest reconnect to it; a
registered run whose owning process died SHALL remain discoverable and
attachable through the shared run registry; and a follower whose forwarded
call was in flight SHALL surface a structured failover error rather than
hanging — idempotent reads MAY be retried once after promotion, mutating
calls MUST NOT be retried automatically. A detached deploy SHALL be owned by
a supervisor process independent of the launching frontend. The supervisor
MUST keep the deploy's event and diagnostic streams writable, reap the child,
and atomically record terminal metadata even when the launcher is killed.

#### Scenario: a deploy survives its leader process
- **WHEN** the leader process is forcibly terminated while a deploy it launched is still running
- **THEN** the deploy and supervisor continue, event and diagnostic streams remain writable, terminal metadata is recorded, and the promoted leader attaches to the same run through the registry

#### Scenario: a promotion race has one winner
- **WHEN** two followers detect leader death and attempt promotion concurrently
- **THEN** the endpoint bind arbitrates: exactly one becomes leader and the other reconnects as a follower

#### Scenario: a supervisor dies before settlement
- **WHEN** both the launcher and a detached deploy's supervisor disappear before terminal metadata is recorded
- **THEN** status inspection reports an explicit orphaned failure rather than a permanently running deploy

### Requirement: Context discovery and authorization
Each context SHALL publish a discovery file in the mandala state directory,
keyed by the canonical checkout, recording the endpoint address, bearer
token, leader pid, and flake path, readable only by the operator (mode
0600). Connections to the endpoint SHALL authenticate with the bearer
token before any other frame is served. Context liveness SHALL be judged by
connecting to the endpoint — recorded pids are advisory only (a recycled
pid must not make a dead context look alive, nor block a claim). A
handshake that *times out* against a port the prober failed to bind SHALL
be treated as a busy live leader — retried on the same port within a
bounded window — never as a dead or foreign endpoint; only a refused
connection or a definitive protocol answer advances the acquisition walk.
When the retry window is exhausted the instance SHALL fail acquisition and
degrade loudly to standalone service rather than bind a second endpoint or
rewrite the discovery file: one checkout never gains a second leader
because its first was slow.

#### Scenario: stale discovery does not block a claim
- **WHEN** a process finds a discovery file whose endpoint refuses
  connections (leader dead, pid possibly recycled)
- **THEN** it claims leadership by binding the endpoint and rewrites the
  discovery file, without requiring manual cleanup

#### Scenario: an unauthorized connection is rejected
- **WHEN** a local client connects without the discovery file's bearer token
- **THEN** the endpoint serves it nothing beyond the authentication failure

#### Scenario: a busy leader is not usurped
- **WHEN** a process attempts to join while the live leader is too busy to
  complete the handshake within the probe timeout
- **THEN** the joiner retries the leader's port within its bounded window and
  either joins when the leader recovers or degrades to standalone — it never
  binds a new endpoint for the checkout and never rewrites the discovery file

#### Scenario: acquisition stays inside the client handshake budget
- **WHEN** an MCP harness launches an instance against a checkout with a
  busy or unresponsive leader
- **THEN** acquisition (including all probe retries) completes or fails fast
  enough that the instance still answers the client's protocol
  initialization within the harness's connect deadline

### Requirement: Observers attach to the live context
The TUI and CLI SHALL discover and use a live context: the TUI attaches as
an observer (rendering the activity stream from every participating
instance) or claims leadership itself when no context exists; the CLI SHALL
route read operations through a live context's warm evaluator and fall back
to local evaluation when none exists, with identical results either way.

#### Scenario: CLI reads get warm answers
- **WHEN** a CLI read command runs while a context leader holds a warm
  evaluator for the same checkout
- **THEN** the result is served through the context — identical in content
  to a local evaluation, without re-paying evaluator warm-up

#### Scenario: no context, no failure
- **WHEN** a CLI read command runs with no live context for the checkout
- **THEN** it evaluates locally and succeeds exactly as the standalone
  binary always has

### Requirement: Leader-owned checkout watching
For a local Git checkout, the fleet context leader SHALL be the sole owner of
commit watching and automatic contract reload. A detected commit change SHALL
be evaluated once through the leader's shared evaluator, atomically swap the
shared inventory on success, and publish the normal reload settlement so every
observer adopts the same inventory. Followers and observers SHALL NOT run
independent checkout evaluations for that change.

#### Scenario: Multiple frontends observe one commit move
- **WHEN** multiple TUI and MCP frontends share a context and the checkout commit changes
- **THEN** the leader performs one automatic reload and every observer adopts its resulting inventory through the shared activity stream

#### Scenario: Observer adopts without recursive reload
- **WHEN** a TUI receives the settlement for a reload already completed by the leader
- **THEN** it re-reads the swapped inventory without invoking the reload tool again

#### Scenario: Leadership changes
- **WHEN** the context leader exits and another participant promotes
- **THEN** the old watcher stops and the promoted leader owns one new watcher with cold evaluator state

