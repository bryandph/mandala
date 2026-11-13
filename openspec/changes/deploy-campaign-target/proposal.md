No public tracking issue.

## Why

A deployment can outlive the checkout state that selected it. Today the native engine pins resolved derivations for a single build/recovery cycle, but the registry does not bind a series of attempts, reboot selections, and final verification to one durable source and per-host expectation. A fresh drift evaluation can therefore describe a new source rather than whether the original deployment landed. Selector exclusions also lose their reasons and continuity when selections are reconstructed.

## What Changes

- Introduce opt-in deployment campaigns that bind a content-identified source, exact resolved membership, intended per-host closures and explicit reasoned exclusions before deployment effects.
- Persist a versioned campaign record separately from existing run directories, with additive attempt references in run metadata and durable, explicitly versioned exclusion updates.
- Retry only failed or pending eligible members against the original target; retain successful outcomes without reactivation. Report source changes and require a new campaign rather than silently evaluating a replacement target.
- Apply campaign exclusions consistently to deploy, retry, reboot and final verification, and distinguish successful deployment, reboot-pending, verification, and administrative closure.
- Expose campaign creation, inspection, retry and close consistently in CLI, TUI and MCP without replacing existing run interfaces.
- Present current-versus-campaign and current-versus-latest-source drift separately; absent expectations never mean clean.

## Capabilities

### New Capabilities

None. A campaign coordinates existing deployment, registry, drift and frontend capabilities rather than introducing a separate fleet subsystem.

### Modified Capabilities

- `fleet-deploy`: bind initial and subsequent attempts to immutable source and closure targets; preserve exclusions and successful host outcomes.
- `fleet-state-formats`: versioned campaign persistence, attempt links, exclusion revisions, outcome reconstruction and retention protection additive to existing run formats.
- `fleet-cli`: CLI/TUI campaign lifecycle and explicitly separated drift comparisons, preserving run attachment and command behavior.
- `fleet-mcp`: additive campaign tools, structured target/source reporting and unchanged real-mutation confirmation gates.
- `fleet-context`: shared campaign mutation serialization and reload isolation so a warm evaluator or context replacement cannot retarget a campaign.

Canonical specs remain untouched during this planning change; synchronize the deltas when archiving after separately approved implementation.

## Impact

Future work touches the native deploy engine, registry, drift expectations, selectors and reboot dispatch in `crates/mandala-core/src/`, plus the CLI, TUI and MCP adapters. See `design.md` for the authoritative code pointers, persistence decisions, alternatives and verification matrix. All files in this change specify future behavior only; artifact completeness is not approval or authorization to implement.

## Non-goals

- Implementation, consumer repository changes, live deployment, reboot, or mutation of existing run records.
- High-availability rollout planning, quorum scheduling, availability budgets, or a fleet-wide rollback transaction.
- Cache publishing changes, new cache trust/credentials, or changes to the publish phase specified in `openspec/changes/deploy-cache-publish/`.
- Replacing deploy-rs per-host rollback, the reboot playbook, existing selector grammar, supervisor ownership or signal handling.
- Automatically adopting a new source into an existing campaign or retroactively inventing expectations for legacy runs.
