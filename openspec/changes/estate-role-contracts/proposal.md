No public tracking issue yet.

## Why

mandala currently contracts the fleet. Organization data and project environments have no shared, validated interface. The intended estate architecture has eight roles: `library`, `profiles`, `operator-profile`, `org`, `fleet`, `operator`, `estate`, and `project`. Publishing all eight interfaces before exercising their consumers would freeze assumptions that have not been proven.

This change proves one complete path with fictional data: org schema → explicit org/project interfaces → proxy dependency graph → NixOS mirrors adapter. Only the org and project role interfaces are introduced. They are experimental in this slice; a future compatibility policy must precede a stable API declaration.

## What Changes

- Add strict org data evaluation for a name, domains, an optional Nix mirrors section (at most 30 entries, explicit priorities, signing keys, labels), and an optional org-level `sites` catalog whose sites select preferred mirrors.
- Define exact org and project output shapes under `mandala.contract.*`, with discoverable role metadata under `mandala.role`.
- Add `roles.org` and `roles.project`, including conformance checks and named input-validation failures.
- Demonstrate a project importing its role module through a neutral proxy. The proxy is a fixture, not a published profiles role contract.
- Separate contract API compatibility from dependency-source agreement. Test both against the resolved dependency graph.
- Deliver one NixOS adapter and prove cache selection with an unprivileged client talking to a Nix daemon.
- Add fictional org, project, proxy, and cache fixtures, including negative cases for malformed data, incompatible interfaces, dependency drift, unknown sites, and missing daemon trust.
- Keep existing fleet exports and consumers unchanged.

## Capabilities

### New Capabilities

- `role-contracts`: experimental org/project interfaces, conformance, input checks, proxy provenance, compatibility and source agreement.
- `org-contract`: validated org data, Nix mirrors, the org-level site catalog and site selection, and the NixOS adapter.

### Modified Capabilities

- `mandala-engine`: the purity invariant changes from "nixpkgs is the only flake input" to "nixpkgs plus the structural inputs `flake-parts` and `import-tree`, all overridable by consumers". Delivery tools and toolchains stay out of the engine's inputs, and `lib`/schemas still depend only on `nixpkgs.lib`.

The existing fleet schema and aggregate are unchanged.

## Impact

New contract schemas, role modules, NixOS adapter, fictional fixtures, and targeted CI checks. The root adds the structural inputs `flake-parts` and `import-tree` (a `mandala-engine` requirement change); delivery tools remain consumer-owned. No private source or live system is required to verify this change.

## Non-goals

- Publishing the other six role interfaces or a production profiles flake.
- Operator identity, memberships, roster validation, attestation, revocation, or multi-org machine composition.
- Per-site settings beyond mirror preference (network, time, proxies), a general service catalog, CA trust distribution, container or language mirrors.
- Org-supplied extension schemas or extension-promotion compatibility shims.
- Home Manager, nix-darwin, or devenv adapters and Workbench machinery.
- Templates for interfaces that have not yet been proven by fixtures.
- Moving the fleet aggregate, changing fleet schemas, or migrating any real estate.
- Stable API guarantees, releases, merges, or deployment without their separate authorization.
