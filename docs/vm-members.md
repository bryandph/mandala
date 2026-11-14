# VM member contract

This is an additive contract, **not yet a deploy-ready runtime backend**. Rust
readers accept aggregate versions 1 and 2 and validate ownership independently.
`mandala deploy run --limit guest-a --preview` displays the resolved owner and
all affected siblings without registering a run, building or contacting a host.
Owner-unit registration and VM-bearing campaigns fail closed until runtime and
campaign qualification are complete. No consumer or live VM is enrolled.

## One identity, one lifecycle owner

Import the member schema in both owner and guest NixOS configurations. Category
is organizational, platform describes OS/venue, and build describes closure
construction. None infers ownership: an existing `category = "vm"` remains
`kind = "standalone"` unless explicitly changed.

A fictional acme guest's configuration authors:

```nix
host = {
  name = "guest-a";
  kind = "vm";
  platform = "nixos";
  vm = { owner = "host-a"; backend = "nixos-module"; };
  build.system = "aarch64-linux";
  deployment.deployRs.enable = false;
  deployment.ansible.enable = false;
};
```

`host.vm` is nullable, requires exactly one canonical owner ID plus the initial
`nixos-module` backend, and is required only for `kind = "vm"`. Both member
schema entry points (`lib.evalMember` and direct NixOS module imports) share
local assertions. An effective true guest SSH enable, including an inherited
`mkDefault true`, is a contradiction naming the guest and owner, not an opt-in.
Factories must set their defaults according to execution kind:

```nix
{ config, lib, ... }: {
  host.deployment.deployRs.enable = lib.mkDefault (config.host.kind != "vm");
  host.deployment.ansible.enable = lib.mkDefault (config.host.kind != "vm");
}
```

## Owner-side application declaration

The consumer's injected runtime module authors this facet **inside the owner's
configuration**, not in a fleet-level registry:

```nix
host.vmApplication = {
  backend = "nixos-module";
  manifestVersion = 1;
  supportedSystems = [ "x86_64-linux" "aarch64-linux" ];
  guests.guest-a.system = "aarch64-linux";
};
```

The owner must be standalone, NixOS, deploy-rs-enabled and have build metadata.
Each guest must be NixOS with build metadata. After collision-free fleet
assembly, `lib.vm.validate` checks all owner/guest relations, configuration and
toplevel attribute availability, backend/version agreement, declared owner and
guest system support, and exact manifest/relation agreement in both directions.
A manifest cannot introduce a second owner, an unrelated guest, an omitted guest
or a different guest system. Self, missing, VM and unmanaged owners, cycles,
nested hosting and unsupported pairs fail before effects. In pure data calls,
configuration availability is checked when `configurations` (or the narrower
`configurationNames`) is supplied; fleet wiring always supplies configurations.
Checking artifact attribute availability does not evaluate or build its value.

The declaration is a pure compatibility summary, **not** the runtime evidence
manifest. The existing owner deploy profile/toplevel remains the application
artifact; no free installable override or independent artifact authority exists.
The eventual runtime manifest must live inside that artifact, reference immutable
guest outputs in the same graph, and supply bounded readiness/recovery contracts.
No virtualization toolchain is pinned or guest system generated here. System
support is an explicit consumer assertion, not inferred emulation or proof of
available build capacity. A macOS owner adapter remains unsupported.

## Identity versus transport projections

VMs remain in aggregate members and canonical groups. Their own network facts
remain intact for DNS/DHCP publication and documentation; an owner's address is
never substituted. Sops reader membership still uses the canonical taxonomy.
Transport projections apply eligibility **after** taxonomy: guests never appear
in Ansible hosts/hostvars/children/guard groups, deploy-rs nodes, independent static
batch entries or builder routes. Direct calls retain those guards even on
malformed input; hostvar hooks are never invoked for excluded guests. Controller
platform variants use the same projections. A guest-only raw batch group does
not expand to its owner; the static batch is still a cache-warming surface.

VM builder facets are forbidden. Guest `build.system/buildPlatform` does not
advertise execution capacity. Guest-backed capacity must be explicitly authored
on the owner's `host.builder` (systems, features and lanes), dispatched by its
daemon, and routed with strict owner-endpoint trust. No guest route, implicit
emulation or duplicate advertisement is inferred. Owner self-exclusion leaves
that capacity local. See [the builder contract](builders.md).

## Pure versioned output and selection

VM-bearing aggregates use `schemaVersion = 2` and include:

- `members.<guest>.kind` and `.vm`;
- sorted, derived `projections.vm.guestsByOwner.<owner>` lists (owners without
  guests are omitted); and
- `projections.vm.applications.<owner>`, derived from the owner's declaration,
  plus `owner` and `application = ".#deploy.nodes.<owner>.profiles.system.path"`.

Application strings reference the existing deploy profile, not derivations.
Aggregate evaluation never evaluates the owner/guest toplevels. The VM projection
is derived, so a projection contribution cannot forge the guest index. Fleets
without VM relations/application declarations retain schema version 1 and omit
new default fields from serialized members, preserving prior aggregate bytes.

`lib.vm.resolve { members; selectedMembers; excludedMembers ? []; }` consumes
already-resolved canonical IDs once. It returns sorted/deduplicated
`selectedMembers`, `requestedGuests`, `skippedMembers` and `applicationUnits`
keyed by owner (or ordinary standalone target). Units carry the owner profile
reference, requested guests, all `affectedGuests` and the unrequested
`potentiallyAffectedGuests`; VM units additionally carry backend/manifest version.
Selecting an owner and any of its guests yields one unit. Disabled guest SSH
enables do not skip ownership resolution; ordinary nondeployable standalone
members are skipped. Explicitly excluding an owner or any affected sibling is a
conflict, including when the owner is selected directly. Unknown IDs are refused.

This function performs no approval or effects. Future frontends must bind impact
and revision to approval, display readiness/recovery and disk/data limitations,
and require renewed approval when the immutable plan changes. Owner host
confirmation alone cannot prove guest runtime success. The optional
[owner runtime helper](vm-owner-backend.md) implements the owner-side
serialization/recovery seam; shared frontend settlement and approval still gate
registration before real VM deployment can be authorized.

## Rust manifest and selection contract

`mandala_core::vm` retains selected members and requested guests separately from
owner units. Owner-plus-guest selections deduplicate the existing owner profile
installable in the native batch build. Excluding an affected sibling or required
owner is an error, including when selecting the owner directly. Ordinary
nondeployable standalone members are still skipped.

The version-1 typed runtime manifest lives at
`mandala/vm-application.json` inside the owner profile. Its owner/system/backend
and exact guest set must match the preview. Each guest declares its runtime
instance, desired identity, immutable artifact, bounded readiness contract and
recovery contract. The adapter and every artifact/contract must be store
references in the independently queried owner closure, not arbitrary shell hooks
or an additional guest build. Readiness deadlines are between 1 and 3600 seconds.
Previous owner profile, running generation, boot default and every affected
ready guest runtime/artifact are required; missing prior runtime refuses creation.
These are validation contracts, not a claim that runtime effects are enabled.
Writable disks, application data and external services are not reverted.

## Tests

`vm-owner-resolution` runs the isolated Rust owner-plan and manifest validation
suite: no owner/guest/network is contacted. It covers version negotiation,
forged aggregates, immutable dependencies, sibling exclusions, deduplication,
approval fingerprint changes and the temporary campaign/runtime fail-closed gates.

`vm-member-schema` tests local/module and fleet validation, including a negative
macOS owner and missing configurations/artifacts. `vm-projections` tests direct
and integrated/controller-platform guards, canonical/publication facts, owner
capacity trust/self-exclusion, pure JSON with poisonous toplevels, owner selection
and exclusions, and exact standalone aggregate/deploy/inventory/batch bytes.
`tests/fixtures/vm-baseline` was captured from the pre-VM implementation; it is
not regenerated from expected new behavior.
