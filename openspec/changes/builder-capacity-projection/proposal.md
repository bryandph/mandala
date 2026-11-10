No public tracking issue.

## Why

A member's closure build instructions do not describe the capacity it offers to
other machines. Controller-local builder lists duplicate capabilities and hide
differences in routing, trust, and scheduling policy. Two controllers running the
same OS may legitimately need different builder sets, keys, and jump routes.

This change specifies the first implementation slice: validated member capacity,
explicit controller profiles, and pure per-controller outputs. The broader
rationale and staged destination are in
[the builder-capacity proposal](../../../docs/proposals/builder-capacity.md).

## What Changes

- Add optional `host.builder` capacity, independent of `host.build`, deployment
  settings, and operator identity; null means no offered capacity.
- Reuse `extraMembers` / `evalMember` for facts-only builders without managed
  closures; retain source collision checks and disabled management surfaces.
- Add `mandala.build.controllers.<id>` with explicit member identity, allowlists,
  optional lane selection, and structured routes carrying absolute string key
  paths and public host trust.
- Validate scheduling fields, member/lane references, route/jump graphs, trust,
  and injection-safe connection data at evaluation time.
- Define replacement-based lanes and overrides, deterministic ordering, and
  self-exclusion by member identity rather than OS or hostname.
- Publish `buildMachinesByController.<id>` lists and JSON-only aggregate
  `projections.builders.byController.<id>` records with structured SSH/knownHosts
  data. Preserve existing aggregate and deploy bytes when nothing is declared.

## Capabilities

### New Capabilities

- `builder-capacity`: builder facet, controller profiles, strict route validation,
  lane normalization, and pure per-controller projection contract.

### Modified Capabilities

None. The new capability composes with the existing `fleet-inventory` member and
aggregate contracts without replacing their requirements. Existing
`fleet-deploy` behavior remains unchanged.

## Impact

A later implementation touches member schema, pure library functions, fleet
wiring, and fake-fleet checks. This change contains specifications and design
documentation only. It adds no live builders, keys, deployment behavior, or
consumer configuration. No private source is required for its tests.

## Non-goals

- Slice 2: NixOS/nix-darwin adapters, machines-file rendering, installed system SSH
  and knownHosts configuration, and deployed-version scheduler qualification.
- Slice 3: consumer enrollment/migration, baseline capture, daemon-context
  connectivity tests, reconciliation of differing lane policies, or deployment.
- Slice 4: raw `mandala deploy run --builders` passthrough, supervisor/run-options
  changes, or CLI/TUI/MCP runtime controller selection.
- Altering `deployment.controllerSettings`, its OS keys, or deploy precedence.
- VM creation/startup, key enrollment/rotation, trust expansion, global resource
  reservations, discovery of reachability, or a separate external-builder registry.
- Binary-cache publication, target-local builds, deploy-rs remoteBuild, or changes
  to the single batch build and effect-free recovery behavior.
- Release, merge, deployment, or implementation authorization implied merely by
  complete artifacts.
