No public tracking issue.

## Why

A VM needs its own member identity without becoming an independent SSH lifecycle target. Organizational `category`, OS/venue `platform`, and closure construction `build` in `schema/member.nix` do not describe ownership. Existing management-enable filters in `lib/projections/` cannot enforce host-owned application or prove a guest is running after a host switch.

## What Changes

- Add `host.kind = "standalone" | "vm"` and a typed `host.vm = { owner; backend; }` relation, validated across the merged fleet, with a derived owner-to-guests index.
- Keep guests in inventory, taxonomy, DNS and documentation while excluding them from SSH Ansible inventory, deploy-rs nodes/batches and builder routes. Reject contradictory SSH opt-ins.
- Resolve guest selections to deduplicated owner application units with affected siblings visible before approval; retain one fleet graph, one batch build and activation from prebuilt owner artifacts referencing guest closures.
- Specify one initial backend, `nixos-module`: a NixOS owner's injected module manages a NixOS guest's runtime. Require observed generation and readiness for deployment success, and verified runtime restoration for rollback.
- Serialize by owner, persist owner/guest recovery evidence and distinguish staged, verified, failed and partially recovered outcomes across frontends.
- Require fake-fleet and effect-isolated recovery tests before implementation ships. Explicitly defer the macOS host-application adapter.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `fleet-inventory`: typed VM ownership, relation validation, derived index and publication versus transport membership.
- `fleet-deploy`: owner-unit resolution, graph/build dependencies, backend guarantees, serialization and honest recovery outcomes.
- `mandala-engine`: pure versioned ownership projections and guards on every projection entry point.
- `fleet-state-formats`: durable ownership, requested/observed runtime and recovery records with compatibility gates.

## Impact

Future implementation affects `schema/member.nix`, `flake-modules/fleet.nix`, `lib/projections/`, fleet aggregation/selection, native deploy orchestration, registry readers/writers and shared CLI/TUI/MCP projections. Consumers supply configurations and the module backend; Mandala does not generate guest systems or pin a virtualization toolchain. Source contracts are in `openspec/specs/` and consumer conventions in `docs/convention.md`.

This change adds planning artifacts only. The selected first-class-member/owner-application model is the design input, not authorization to implement these detailed interfaces. Artifact completeness is not approval: obtain plan review and explicit approval before implementation.

## Non-goals

Implementation; guest SSH or guest-exec backends; nested hosting; consumer migration or rollout; automatically inferred builder capacity; VM disk/data snapshots; Kubernetes node-identity inference; a macOS adapter or a claim that today's NixOS-only deploy projection can apply a macOS owner.
