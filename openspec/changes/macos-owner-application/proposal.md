## Why

The `vm-member-host-application` change establishes VM identities applied through directly managed owners, but intentionally defers macOS owners. A macOS workstation owning a Linux builder VM needs both a deployable owner unit and owner-side evidence of guest application; a facts-only member and NixOS-only deploy projection cannot provide either.

## What Changes

- Add opt-in enrollment from consumer `darwinConfigurations` to the same collision-free fleet graph as NixOS configurations and extra members.
- Define a nix-darwin owner system profile with a typed application manifest, prebuilt Linux guest dependencies and a Darwin-native deploy-rs activation wrapper.
- Extend the VM backend enum with `darwin-module`, retaining owner-only transport, impact approval, cross-controller serialization and owner-only builder capacity.
- Specify owner-side running identity/readiness evidence, retained-runtime recovery and honest outcomes when re-imaging replaces disks and keys.
- Preserve controller-platform connection identities for both self-deployment and remote deployment; qualify behavior with effect-isolated fixtures.

## Dependencies and Impact

`vm-member-host-application` is now archived and applied to the canonical specs; this change's deltas therefore apply against those canonical requirements.

This change follows and modifies the proposed requirements in `../vm-member-host-application/`; it does not replace that change or implement either plan. Its deltas are applied after that change's deltas. In particular it extends backend validation and consumer-injected artifacts, replaces the single-backend restriction, and scopes NixOS boot/preflight guarantees rather than pretending they are Darwin operations.

Implementation would touch the entry points named in `design.md`, the native deploy adapter and state readers. All files in this change are planning artifacts; existing code and consumer outputs are unchanged.

## Non-goals

Implementation, consumer migration, guest SSH, guest-exec, nested hosting, automatic first-VM bootstrap, writable-data restoration, arbitrary macOS administration, new virtualization dependencies, and rollout or live runtime qualification. No public tracking issue exists for this change. Artifact completeness is not approval to implement.
