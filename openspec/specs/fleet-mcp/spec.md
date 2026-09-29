# fleet-mcp Specification

## Purpose
The `mandala mcp` stdio server: read, drift, deploy-monitoring and confirmation-gated action tools over the fleet inventory, reusing the same cores as the CLI and TUI so tool behavior stays at parity across frontends and implementation swaps.

## Requirements
### Requirement: MCP server over the inventory, reusing the cores
The system SHALL provide an MCP server, packaged in the mandala repo, that
exposes fleet read and action operations by reusing the existing porcelain
cores (`inventory`, `drift`, `runner`) and the same action paths the CLI/TUI
use, and contains no orchestration logic of its own. It SHALL resolve members,
groups, and selectors from the same versioned aggregate `flake.mandala` output
the CLI reads. The server SHALL NOT surface effect engines of its own; engine
dispatch remains a CLI concern (external subcommands), and the server's action
tools invoke the shared playbook/wrapper paths directly.

#### Scenario: a selector resolves to the same members the CLI projects
- **WHEN** an MCP client resolves an `@group` (or comma-list) selector
- **THEN** the server returns exactly the member names the inventory projects
  for that selector — identical to `mandala resolve` and the ansible `--limit`

#### Scenario: the server adds no orchestration
- **WHEN** the server answers any read or action request
- **THEN** it does so through the `inventory`/`drift`/`runner` cores and the
  same playbooks the TUI/CLI run, not by evaluating nix or invoking ansible
  through a path the CLI/TUI do not share

### Requirement: Read tools at parity with the read-only tier
The server SHALL expose read operations — member list, group list, selector
resolution, per-host eval information, and reachability (ping) — that only read
or probe fleet state and never mutate it. Per-host eval information SHALL always
include the host's aggregate metadata; the slow evaluated `toplevel` out-path
SHALL be computed only when the request explicitly asks for it.

#### Scenario: read operations cannot mutate state
- **WHEN** an MCP client calls any read tool or reads any resource
- **THEN** no deploy, reboot, push, survey, or write action is issued

#### Scenario: host eval is cheap by default
- **WHEN** a client requests host eval information without asking for the
  toplevel
- **THEN** the server returns the host's aggregate metadata and runs no nix
  evaluation

#### Scenario: ping probes reachability without mutating
- **WHEN** a client pings a selector
- **THEN** the server runs the ansible reachability probe and reports per-host
  reachability, changing no fleet state

### Requirement: Drift reporting
The server SHALL expose deployed-generation drift as the same
`DriftStatus`/`DriftEntry` judgement the dashboard uses, including
`reboot-pending` when the installed system profile equals the expected
generation while the running generation differs. Exact-path equality and
distinct stale, incomplete, never-surveyed, and unreachable statuses SHALL
remain shared with the CLI/TUI core. The expensive inputs — the read-only
state survey and expected-toplevel evaluation — SHALL run only when the
request explicitly opts into them; a plain drift read uses existing snapshots
and the rev-keyed expectation cache without surveying or re-evaluating. A
requested refresh that exits non-zero MUST be returned as a failed refresh
with its exit code and captured stdout/stderr; existing snapshots MAY be
included for diagnosis but MUST NOT be labeled freshly refreshed.

#### Scenario: a plain drift read is side-effect-free
- **WHEN** a client reads drift without requesting a refresh or eval
- **THEN** the server reports drift from existing snapshots and the cached expectation without surveying or re-evaluating

#### Scenario: staged expected generation is reported
- **WHEN** an existing snapshot records expected as the system profile but not the running generation
- **THEN** the server returns reboot-pending, matching the dashboard

#### Scenario: opting into a refresh runs the read-only survey
- **WHEN** a client requests refresh and the survey succeeds
- **THEN** the returned judgement incorporates the freshly observed current, booted, and system-profile targets

#### Scenario: a failed refresh preserves diagnostics
- **WHEN** the requested survey exits non-zero
- **THEN** the tool reports failure with the survey exit code and captured diagnostics and does not label existing snapshots freshly refreshed

#### Scenario: drift never redefines the judgement
- **WHEN** a member's snapshot is older than the staleness threshold
- **THEN** the server reports it as stale, matching the dashboard, not as
  in-sync, reboot-pending, or drift

### Requirement: Deploy monitoring across frontends
The server SHALL report live and recent deploy state — per-host states and
build progress — by attaching to the shared deploy run registry, so a deploy
launched from any frontend (TUI, CLI, or MCP) is observable. Reported per-host
state SHALL come from sticky terminal protocol states: confirmed,
reboot-pending, rolled-back, and failed. Reboot-pending SHALL remain visible
as successful staging that still requires reboot, while failed and rolled-back
retain failure precedence. The run listing (no run id given) SHALL return light
per-run summaries — identity, liveness, phase, per-host states — with no build
graph and no raw streams. Assembling a status snapshot SHALL NOT block the
server's message loop or the context coordination endpoint: registry tailing
and snapshot construction run off the async loop so concurrent calls and
joining instances are served while a status poll grinds through a large run.

The single-run form (run id given) SHALL default to a summary-first shape: run
identity, liveness, and phase; curated run metadata (deploy summary counters,
effective and process exit values, target and options, timestamps — no
per-host store paths); per-host sticky states and exit values; and raw output
tails only for failed or rolled-back hosts. Build-forest detail and per-host
diagnostic detail (milestone streams, the raw run metadata including store
paths, extended command-run output) SHALL each require an explicit per-call
opt-in.

A blocking status wait SHALL be capped so a maximal wait completes within a
single MCP client call budget rather than a limit the transport cannot honor.
A wait that elapses while the run is still live SHALL be explicitly marked as
timed out in the response, distinguishable from a settled run; the intended
long-wait pattern is re-invocation against the run registry.

#### Scenario: a TUI-launched deploy is visible to the MCP client
- **WHEN** a deploy launched from the TUI contains a reboot-pending host and an MCP client queries it
- **THEN** the response includes that host's reboot-pending state and preflight/staging stream from the shared registry

#### Scenario: terminal states are honored
- **WHEN** a host reaches confirmed, reboot-pending, rolled-back, or failed
- **THEN** a late non-terminal event does not replace that terminal state, except that rollback retains its existing precedence

#### Scenario: the listing stays light
- **WHEN** a client lists recent runs without naming one
- **THEN** each entry carries identity, liveness, phase, and per-host states
  only — no build graph, no raw streams — regardless of how large the
  underlying runs are

#### Scenario: a status poll does not wedge the server
- **WHEN** a client blocks in a deploy-status wait over a large run while a
  second client issues reads and a new instance joins the context
- **THEN** the concurrent reads are answered and the joiner completes its
  handshake while the wait continues polling

#### Scenario: a finished fleet-scale deploy answers "did it work" cheaply
- **WHEN** a client reads the terminal status of a completed fleet-scale
  deploy without opting into build or diagnostic detail
- **THEN** the response carries the summary shape — outcome counters,
  per-host terminal states, and raw tails for failed or rolled-back hosts
  only — with no activity rows, no recent-log ring, no milestone arrays, and
  no per-host store paths

#### Scenario: diagnostic detail remains one flag away
- **WHEN** a client re-queries the same run with the diagnostic opt-in
- **THEN** the response adds the per-host milestone streams and the raw run
  metadata, including per-host store paths, that the default shape omits

#### Scenario: a wait that outlives its budget is marked timed out
- **WHEN** a client waits on a live run and the capped wait elapses before the
  run settles
- **THEN** the returned snapshot reports the run still live and carries an
  explicit timed-out marker, and re-invoking with the same run id resumes
  observation without loss

### Requirement: Tiered confirmation-gated action tools
The server SHALL expose the mutating action tiers the TUI already runs — build,
deploy, and reboot — as tools gated by blast radius. A **build** tool SHALL
`nix build` the resolved `toplevel`(s) without activating and MAY run without a
confirmation argument (it changes only the local store). A **deploy** tool SHALL
dispatch through the native deploy engine, defaulting to dry-activate; a real
(non-dry) activation SHALL require an explicit per-call confirmation argument
naming the resolved target. A **reboot** tool SHALL dispatch through
`playbooks/reboot.yaml` with the same serial-order and k8s-drain options the
TUI offers, and SHALL require the same naming confirmation argument. No action
tool SHALL bypass the engine guards — the resolved `--limit`, throttle, and
per-host deploy-rs magic rollback remain in force exactly as for the CLI and
TUI.

#### Scenario: build and dry-activate need no confirmation
- **WHEN** a client invokes the build tool, or the deploy tool without the
  confirmation argument
- **THEN** the build runs (local store only) or the deploy runs dry-activate,
  and no real activation or reboot occurs

#### Scenario: a real deploy or reboot requires a matching confirmation
- **WHEN** a client invokes the deploy tool for a real activation, or the
  reboot tool
- **THEN** the tool proceeds only if the confirmation argument matches the
  resolved target, and otherwise refuses without launching a run

#### Scenario: engine guards are never bypassed
- **WHEN** an action tool launches a run
- **THEN** the deploy tool launches a native-engine run with the resolved
  `--limit` and throttle, leaving per-host magic rollback in force, and the
  reboot tool runs its playbook — neither invokes activation or reboot
  machinery directly

### Requirement: Subordinate-tool error surfacing
The server SHALL surface subordinate-tool failures as structured tool output.
When a tool dispatches a subordinate tool (nix build/eval, ansible, deploy-rs,
the reboot/survey playbooks) and that subordinate fails, the tool MUST return a
failed status, the exit code, and the captured diagnostic output
(stderr/stdout, or for a run the failure event and a bounded tail of the
failing host's raw stream together with the elided line count and the run's
on-disk stream location) — rather than a bare boolean or an opaque transport
error — so the client can diagnose and troubleshoot it.

#### Scenario: a failed build returns the nix error
- **WHEN** a build or toplevel eval the server dispatched exits non-zero
- **THEN** the tool result reports failure with the captured nix error output and the command/target that produced it, not just a failure flag

#### Scenario: a failed deploy returns the failing host's diagnostics
- **WHEN** a deploy run the server launched ends with a host failed or rolled-back
- **THEN** the tool result (or deploy-status query) reports that host's
  terminal state together with a bounded tail of its raw stream, the count of
  elided lines, and where the full stream lives, so the client can debug the
  cause without the response growing unboundedly

#### Scenario: a failed survey returns the survey diagnostics
- **WHEN** a drift refresh survey exits non-zero
- **THEN** the drift tool reports failure with the survey exit code and captured stdout/stderr

### Requirement: Live operator observation of MCP calls
The TUI SHALL provide an opt-in debug view of the fleet execution context's
tool calls, enabled by the `--debug-mcp` flag: each call flowing through
the context the TUI hosts or observes — the tool name, arguments,
originating client, the gate decision (allowed / gated / refused), and the
result or surfaced error — regardless of which participating instance
served the client. Without the flag the TUI SHALL render no call-monitoring
view (and no binding for one). Independently of the flag, a mutating action
a client triggers SHALL render in the same deploy/reboot/build views a
human-initiated action renders in, attached via the shared run registry —
run rendering is normal operation, not diagnostics.

#### Scenario: a tool call appears in the debug activity view
- **WHEN** the TUI runs with `--debug-mcp` and a client calls any tool
  through any instance participating in the attached context
- **THEN** the TUI shows that call — name, arguments, origin, gate
  decision, and result/error — in its activity view as it happens

#### Scenario: without the flag there is no monitoring surface
- **WHEN** the TUI runs without `--debug-mcp` while clients call tools
  through the context
- **THEN** no activity view or toggle binding is present, while
  client-triggered runs still render in the deploy/reboot views

#### Scenario: a client-triggered deploy renders like a human one
- **WHEN** a client launches a deploy or reboot through the context
- **THEN** the run renders in the TUI's per-host deploy/reboot view and
  build pane exactly as an operator-launched run would, attached via the
  shared run registry, with or without `--debug-mcp`

### Requirement: Tool-surface parity across implementation swaps
When the MCP server implementation is replaced, the stdio tool surface SHALL
be preserved at parity: the same tool names (`members`, `groups`, `resolve`,
`ping`, `host_eval`, `drift`, `reload`, `deploy_status`, `build`, `deploy`,
`restart_service`, `reboot`), the same argument names and defaults, the same
result shapes (including structured, non-raised subordinate-command errors and
refusal objects carrying `required_confirm`), the same tiered confirm-gate
semantics (reads/ping/build/dry-deploy ungated; real deploy, reboot, and
restart_service require `confirm` equal to the resolved `--limit`), and the
same audit behavior (mutating settles appended to the per-user audit log with
timestamps, transport-independent).

#### Scenario: a recorded client interaction replays against the new server
- **WHEN** a tool call recorded against the prior implementation is replayed
  against the replacement
- **THEN** the result matches in shape and semantics (fields, refusal
  structure, confirm-gate outcome), differing only in incidental values such
  as run ids and timing

#### Scenario: a refused mutation leaves an identical trail
- **WHEN** a gated action is called with a missing or mismatched `confirm`
- **THEN** the replacement server refuses without launching anything and
  returns the resolved `required_confirm`, exactly as the prior implementation
  did

### Requirement: Stdio is the only MCP transport
The system SHALL expose MCP over stdio only, as the `mandala mcp`
subcommand launched by each client harness; no HTTP or network MCP endpoint
SHALL exist. Concurrent stdio instances against one checkout SHALL share
one execution context per capability `fleet-context` (first is leader,
later ones proxy execution), so that any number of harnesses observe one
inventory, one activity stream, and one audit trail. The tool/resource set
SHALL be defined once, independently of leader/follower role.

#### Scenario: two harnesses share one context
- **WHEN** two agent harnesses each launch `mandala mcp` against the same
  checkout
- **THEN** both serve the identical tool set over their own stdio pipes,
  execution flows through the single leader, and both harnesses' calls
  appear in the one activity stream and audit log

#### Scenario: no network MCP endpoint listens
- **WHEN** `mandala mcp` or `mandala tui` runs in any role
- **THEN** no MCP protocol endpoint is reachable over the network — the
  only listener is the context coordination endpoint, which is
  loopback-bound, bearer-guarded, and not an MCP transport

### Requirement: Run registry identifiers are confined
Every MCP operation that attaches to a run MUST accept only a valid Mandala-generated run identifier and MUST resolve it to a direct child of the canonical run-registry directory. Absolute paths, separators, traversal components, and malformed identifiers SHALL be rejected before filesystem access.

#### Scenario: traversal is rejected
- **WHEN** a client requests deploy status with an absolute path, `..` component, separator, or malformed run identifier
- **THEN** the server returns a structured invalid-identifier error and reads no files outside the run registry

#### Scenario: a generated identifier remains attachable
- **WHEN** a client requests deploy status with an identifier returned by a Mandala launch operation
- **THEN** the server attaches to that direct registry child and returns its status

### Requirement: MCP deploy status is rollback-aware
MCP deploy monitoring SHALL derive terminal liveness and success from the same effective fleet result as the CLI/TUI. Any sticky failed or rolled-back host MUST yield non-success even when run metadata contains a zero controller exit. Reboot-pending SHALL be successful, returned distinctly from confirmed, and included in summary counts and diagnostics indicating that a reboot is required.

#### Scenario: status reads a mixed-result run
- **WHEN** `deploy_status` reads a completed run with `rc: 0`, one confirmed host, and one rolled-back host
- **THEN** it returns non-success terminal liveness and includes the rolled-back host's state and raw diagnostics

#### Scenario: status reads a successful staged run
- **WHEN** `deploy_status` reads a completed run with confirmed and reboot-pending hosts and no failure state
- **THEN** it returns success with separate confirmed and reboot-pending counts and identifies the hosts awaiting reboot

### Requirement: Structured build progress reporting
The server SHALL report build progress structurally — derived from the same
internal-json records fed to the TUI's build renderer, never by scraping
renderer output. The default status response SHALL carry bounded scalar
build counters (built, finished, fetched, errors, done, exit value). The
full structured summary — counts by derivation status, failed derivation
names, current activity, bounded recent per-derivation logs, active
transfer endpoints and progress, activity-view rows with elision counts,
and historical ETA estimates when available — SHALL be returned when the
request opts into build detail. The per-node derivation graph SHALL be
returned only on its own further opt-in, capped at a fixed node budget with
active nodes prioritized and an explicit truncation count when elided. No
`deploy_status` response shape SHALL grow unboundedly with run size:
per-node log tails and the uncapped node list are never inline in the
default response.

Derivation-status counts SHALL label graph nodes that never received an
observed activity as untracked — a coverage note, not a failure bucket —
and SHALL exclude them from headline completion denominators, so a summary
over a partially-observed graph does not read as broken.

#### Scenario: an agent reads build state mid-run
- **WHEN** an MCP client queries deploy/build status during a batch build
  with the build-detail opt-in
- **THEN** the response carries derivation counts by status, recent owned log
  tails, active transfer progress/endpoints, available ETA estimates, and any
  failed derivation names, updating as the run progresses — without the
  per-node graph

#### Scenario: the node graph is opt-in and capped
- **WHEN** a client requests deploy status with the forest-nodes option on a
  fleet-scale run whose graph exceeds the node budget
- **THEN** the response includes the capped node list with active
  (building/failed/downloading) nodes prioritized and reports exactly how
  many nodes were elided

#### Scenario: a fleet-scale terminal snapshot stays deliverable
- **WHEN** a deploy of the whole fleet completes and a client reads its
  terminal status without opting into the node graph
- **THEN** the response serializes well under the stdio transport's
  single-message limits and the conversation continues — the server is
  never disconnected for flooding its stdout

#### Scenario: a fleet-scale response explains elision
- **WHEN** the complete build graph exceeds the default activity row budget
  and a client has opted into build detail
- **THEN** the structured activity projection reports which completed or
  untracked nodes were elided while retaining exact full-forest counts

#### Scenario: unobserved nodes do not read as failures
- **WHEN** a build summary covers a graph where many nodes were materialized
  only as dependency edges and never emitted an activity
- **THEN** those nodes are counted under the untracked label, appear in no
  failure or error bucket, and do not inflate the completion denominator

#### Scenario: frontends agree on build events
- **WHEN** the TUI and an MCP client observe the same run
- **THEN** the build pane and the structured MCP summary consume the same
  versioned internal-json event records without MCP depending on terminal
  output

### Requirement: Reload invalidates all evaluator state
The `reload` operation SHALL discard all Nix evaluation state capable of
serving memoized values from the previous checkout commit before evaluating a
replacement inventory. A successful result SHALL mean the shared inventory was
evaluated from the checkout state visible at reload time and swapped
atomically; a failed evaluation SHALL leave the previous inventory available.
The MCP tool name, arguments, and result shape SHALL remain compatible.

#### Scenario: Reload after a commit move
- **WHEN** a worker has evaluated commit A, the checkout moves to commit B, and reload is requested
- **THEN** the replacement inventory contains commit B's contract values and no memoized values from commit A

#### Scenario: Reload evaluation fails
- **WHEN** the moved checkout cannot be evaluated successfully
- **THEN** reload returns the normal structured tool error and the previously served inventory is not replaced

### Requirement: Fleet-wide selection and group discovery are cheap
The server SHALL accept the fleet-wide selector in both spellings (`all` and
`@all`), resolving to every member. A selector naming an unknown group SHALL
fail with an error that enumerates the available group names, so recovery
requires no separate taxonomy call. The group-listing tool SHALL accept a
filter so a targeted lookup returns only matching groups rather than the
entire taxonomy. The selector tool's description SHALL document the full
selector algebra (groups, members, the fleet-wide keyword, list separators,
and exclusions).

#### Scenario: the natural fleet-wide spelling works
- **WHEN** a client resolves `@all` (or `all`)
- **THEN** resolution returns every member, and the two spellings produce
  identical member sets and confirm strings

#### Scenario: an unknown group error is self-healing
- **WHEN** a client resolves a selector naming a group that does not exist
- **THEN** the error names the unknown group and lists the available group
  names, without requiring a group-listing call

#### Scenario: a filtered group lookup stays small
- **WHEN** a client lists groups with a filter matching a few group names
- **THEN** only the matching groups and their members are returned

### Requirement: Mutating-tool descriptions are proportional to blast radius
Descriptions of mutating tools SHALL lead with what the tool does and its
bounds (scope, validation, confirm gate), presenting the execution mechanism
as detail rather than headline, and SHALL read proportionally to actual
blast radius — a single-unit service restart is presented as a smaller
action than a host reboot, never a scarier one. Operator documentation
SHALL note the recommended harness allowlist treatment for the confirm-gated
middle-verb tools.

#### Scenario: the middle verb reads smaller than the big hammer
- **WHEN** the service-restart and reboot tool descriptions are compared
- **THEN** the service-restart description leads with its bounded scope
  (one validated unit, confirm-gated, bounded concurrency) and does not
  front-load mutation warnings absent from the description of the more
  destructive reboot tool

### Requirement: MCP deploy exposes boot mode without weakening confirmation
The MCP deploy tool SHALL accept an additive `boot` boolean defaulting to
false, pass it to the native deploy engine, and return it in refusal and launch
results. Boot mode SHALL NOT change the `dry_activate=true` compatibility
default. Every non-dry deployment SHALL require `confirm` equal to the resolved
target whether boot mode is true or false.

#### Scenario: boot mode with the safe default remains dry
- **WHEN** a client requests `boot=true` and omits `dry_activate`
- **THEN** the tool launches a dry boot-mode run without requiring confirmation
  and performs no activation

#### Scenario: real boot-mode deployment is confirmation-gated
- **WHEN** a client requests `boot=true` and `dry_activate=false`
- **THEN** the tool refuses without launching unless `confirm` equals the
  resolved target, and a matching confirmation launches the boot-mode run

#### Scenario: status reports boot intent
- **WHEN** a client queries a run launched in boot mode
- **THEN** the launch result and curated or diagnostic deploy status identify
  `boot=true`
