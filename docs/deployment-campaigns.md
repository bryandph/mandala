# Deployment campaigns

Campaigns are an explicit opt-in lifecycle beside ordinary deploy runs. Existing
`deploy run`, TUI deployment, run attachment and MCP deploy calls do not create a
campaign. The protocol and decisions are described in
[the campaign design](../openspec/changes/deploy-campaign-target/design.md).

## Enablement and retention gate

Before creating the first campaign, upgrade **every writer of the shared state
directory**, including standalone CLIs, TUI processes and context/MCP leaders.
Stop older processes and restart warm contexts with the same upgraded version.
Do not mix an older retention writer with campaign-aware writers: an older
binary cannot understand the new protection contract. This ordering is a
precondition for enabling campaigns, not an automatic consumer migration.
Obtain separate rollout approval before switching consumers or operating a live
fleet; installing this implementation does not grant that approval.

Open campaigns retain their source snapshots, required derivations, realized
outputs and linked run evidence across normal count/age pruning. They consume
disk until explicitly closed. Inspection lists open campaigns and their target
identities; their `roots` directories hold the retained GC roots. There is no
silent lease expiry. Close does not immediately delete records or unroot live
work. Closed campaigns and their inactive runs become one retention unit, aged
from close time, under the existing keep/age controls. Live or supervised
allocations still prevent pruning. Independent legacy runs keep their existing
retention behavior.

## Create and review

A real creation captures and binds the target, then starts its first deployment
in the **same operation**. It requires confirmation of the exact eligible,
comma-joined names, not an unevaluated group selector. For example, on a fictional
fleet whose selected eligible members are `node-a` and `node-b`:

```sh
mandala campaign create --limit node-a,node-b \
  --actor operator-alice --confirm node-a,node-b
```

An omitted or incorrect confirmation refuses host work and reports the required
names, generation and target/source identity. A CLI run publishes its ordinary
registry run id before host effects, then prints its result; follow that id with
the existing attachment/status interfaces. Failed attempts remain historical.

For an explicit review-only path:

```sh
mandala campaign create --limit @fleet --preview --actor operator-alice
```

This binds and displays the target, with no attempt, build/copy dispatch,
activation or reboot. Source capture and GC root registration are local Nix
operations. The bound campaign remains open with real work pending. Inspect its
paginated detail and later use retry to dispatch that pending work.

**Creation preview is not dry activation.** `campaign retry --dry-activate`
builds/copies eligible profiles but does not activate them; those host effects
are real even though deployment remains pending. MCP `campaign_create` defaults
to `dry_activate=true` as a bind/show preview without an attempt. MCP
`campaign_retry` retains the ordinary dry-activation default. Set
`dry_activate=false` with exact confirmation for real deployment. Neither kind
of preview is successful real deployment or final verification.

In the TUI, `C` opens Campaigns beside Runs. Select members in the explorer first,
then `c` captures an **unbound** target for review. `y` binds and starts the first
attempt; `p` binds only the explicit preview; Esc discards the unbound capture.
The captured object is retained through review rather than recaptured after
confirmation. Boot, halt-on-build-failure and cache controls are shown before
capture/review. Inspection, retry, reasoned exclusion changes, reboot, verify
and close remain separate actions. `a` attaches the selected attempt through the
normal run interface, including settled attempts.

## Immutable source and membership

Capture uses a fresh, pure evaluation of a frozen input graph, including
recursive local-input and source-relevant submodule content. Original trees stay
unchanged. A capsule replays the lock graph over fixed-NAR archived paths and
preserves the root/input fetcher metadata visible to flake code (`rev`,
`dirtyRev`, timestamps, revision counts and source/flake paths). Its semantic
graph identity includes that metadata, not the capsule implementation bytes.
Capture currently supports Nix 2.34.x's sourceInfo override contract; other Nix
versions refuse rather than silently evaluating with changed path metadata.

An always-on pure comparison checks every selected member's profile **and**
system drvPaths from both live and frozen evaluation. A mismatch refuses binding,
names the members and reports the unreproduced fetcher-metadata dependency. This
comparison never substitutes a live target for the frozen one. Dirty content is
identified by content and influencing metadata, not just a revision plus a dirty
boolean. Files Nix excludes are not invented as source inputs.

Required submodules must be initialized and included by the chosen Nix source
reference. For a Git source that needs them, use the fetcher's `submodules=1`
option. An omitted submodule, inconsistent capture, uncaptured impurity or an
unknown intended output refuses binding. Source references must not embed
passwords, usernames carrying credentials or access tokens; configure fetcher
authentication separately. Durable references accept Nix identity/selection
query parameters, not opaque authentication/download queries.

A source change during capture publishes no ready campaign and starts no host
work. A later source mismatch or unavailable identity refuses new deploy/retry/
reboot dispatch and requires a new campaign. An already-running attempt may
finish from its recorded snapshot, derivations, paths and settings. There is no
pinned-retry override, reopening or rebinding. Bound inspection and verification
do not need the original checkout.

Membership is exact and immutable. Selector exclusions retain concrete names
and reasons, and selected non-deploy-capable members retain separate capability
notices. Retry selects failed, rolled-back or pending eligible members only.
Confirmed and successfully staged members are not built, copied or activated
again. An optional retry selector is resolved against bound membership or the
frozen inventory, never latest group membership, and cannot widen the campaign.
An empty retry creates no run.

## Inspect, policy updates and recovery

Use returned ids, generations and resolved names rather than the illustrative
values from another attempt:

```sh
mandala campaign inspect --json
mandala campaign inspect "$id" --check-source --json
mandala campaign retry "$id" --generation "$generation" --confirm "$eligible_limit"
mandala campaign exclude "$id" --generation "$generation" \
  --add node-b --actor operator-alice --reason maintenance --confirm "$id"
mandala campaign exclude "$id" --generation "$generation" \
  --remove node-b --actor operator-alice --reason resumed --confirm "$id"
```

Inspection pages membership, intended paths, source inputs/submodules, attempts
and verification results with totals and continuation offsets. Source status
can be unchecked, matched, moved or unavailable; unavailable checkout identity
does not hide the recorded target. An unsupported manifest version is
identifiable but cannot be mutated or judged clean.

Policy updates require the reviewed generation, actor, nonempty reason and
campaign-id confirmation. They append history, never replace membership,
targets or successful outcomes. Capability notices cannot be removed as operator
skips. Mutating dispatch, policy updates and close share durable interprocess
locks. A stale generation requires fresh review; no lock is held while waiting
for a TUI confirmation. Exclusions do not cancel already-scheduled work.

If a response is lost or ownership is uncertain, inspect first. Do not blindly
replay lifecycle writes. Explicit reconciliation inspects owner/supervisor and
registered terminal evidence without launching host work:

```sh
mandala campaign reconcile "$id"
```

MCP offers the same operation as
`campaign_retry(campaign_id=..., generation=..., reconcile_only=true)`. Live
owners, missing evidence and uncertain outcomes remain blocking. Reconciliation
never infers host success just from a zero controller exit or restarts an old
attempt. After reconciliation, retry is a separate explicit request.

## Reboot, verify and close

Campaign reboot uses the captured fleet reboot wrapper, not a current-checkout
replacement or a raw playbook fallback. A fresh survey must prove that each
eligible installed profile matches the bound system target. Already-running
members are not rebooted again; unknown/mismatched members are reported blocked.
Reboot keeps serial/rolling/all-at-once controls and defaults to drain enabled;
`--no-drain` is an explicit disable.

```sh
mandala campaign reboot "$id" --generation "$generation" \
  --serial 1 --confirm "$eligible_limit"
mandala campaign verify "$id"
mandala campaign close "$id" --generation "$generation" \
  --actor operator-alice --reason completed-review --confirm "$id"
```

Verification surveys eligible members, including earlier successes, without
copy, activation or reboot. An open campaign registers a linked read-only
`verify` run and records generation-bound evidence. The run's
`verification.json` retains the full report; ordinary run attachment exposes its
log. `--no-refresh` compares existing snapshots under the same freshness rules.
Absent/stale/unreachable/incomplete observations and missing boot proof never
establish completion. Excluded rows/reasons remain outside eligible denominators.
A policy-generation change during the survey invalidates a current-completion
claim without erasing the captured evidence.

Close is administrative and may retain failed, excluded or reboot-pending work.
It refuses live/uncertain host mutation and never asserts success. A closed
campaign permits only bound comparison and inspection: verify does not start a
new survey, allocate a run or write the manifest. If close races an existing
read-only verification, its ordinary run/report stays observable, but the closed
manifest is not reopened or rewritten. Follow that run for its final evidence.

## Two drift questions

```sh
mandala drift --campaign "$id" --json
mandala drift --campaign "$id" --eval --json
```

The `campaign` basis compares current observations with recorded system-toplevel
paths, not deploy-profile wrappers. The `latest_source` basis is separately
labeled with its own identity and expectation provenance. A legacy root-revision
cache cannot prove recursive content identity: without an explicit frozen
latest-source evaluation, this basis reports `expectation-unknown`, not clean.
Partial evaluations preserve per-host errors. Cache refreshes never retarget a
campaign, and post-success drift never grants permission to redeploy a previously
successful member through retry. Staging/reboot-pending, current live state,
verification and administrative closure remain distinct.
