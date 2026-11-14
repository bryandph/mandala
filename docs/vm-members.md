# VM member contract

This additive contract has an effect-isolated, opt-in `nixos-module` owner
backend. Rust readers accept aggregate versions 1 and 2 and validate ownership
independently. `mandala deploy run --limit guest-a --preview` displays the owner,
all affected siblings and `requiredOwnerApproval` without registering a run,
building or contacting a host. VM work requires this exact approval fingerprint,
also for dry activation, and a separately qualified consumer adapter. No consumer
or live VM is enrolled by this change.

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

## Runtime manifest schema (version 1)

The injected consumer module writes `mandala/vm-application.json` into the owner
application profile. Every field below is required; Mandala validates these
values and never infers a guest, system, runtime instance or immutable identity.

| Field | Type and meaning |
| --- | --- |
| `manifestVersion` | Integer `1`; unsupported versions fail closed. |
| `owner` | Canonical standalone NixOS owner member ID. |
| `backend` | Exactly `"nixos-module"`. |
| `ownerSystem` | Exact owner's `host.build.system`, included in supported systems. |
| `ownerGeneration` | Immutable NixOS toplevel store path expected running after live activation; distinct from a deploy-rs wrapper profile. |
| `supportedSystems` | Array of explicit owner/guest system strings, exactly matching `host.vmApplication.supportedSystems`. |
| `adapter` | Immutable store executable implementing the typed owner-local adapter protocol. |
| `guests` | Object keyed by exactly the affected guest IDs; no extra, omitted or unrelated guest. |
| `guests.<id>.system` | Exact guest system from the pure facet and ownership relation. |
| `guests.<id>.desiredIdentity` | Immutable running generation store path or lowercase `sha256:` plus 64 hexadecimal digits for an image. Mutable aliases are forbidden. |
| `guests.<id>.artifact` | Prebuilt closure/image store reference, a dependency of this owner artifact. |
| `guests.<id>.runtimeInstance` | Nonempty owner-local runtime instance identity. |
| `guests.<id>.readiness.contract` | Immutable owner-local runtime/service probe contract; never guest SSH. |
| `guests.<id>.readiness.deadlineSeconds` | Integer from 1 to 3600. A failed live restart/deadline is failure, not pending. |
| `guests.<id>.recoveryContract` | Immutable runtime restoration contract reference, not a writable-data rollback promise. |

The adapter, owner generation, artifacts and contract references must occur in
the independently queried owner closure. The helper also compares the copied
artifact's manifest to the approved manifest. `ManifestError.field` identifies
the failing field, for example `guests.guest-a.readiness.deadlineSeconds` or
`guests` for relation disagreement. Additive fields remain ignorable; version
changes and missing evidence do not.

A fictional two-guest JSON example (store hashes are illustrative):

```json
{
  "manifestVersion": 1,
  "owner": "host-a",
  "backend": "nixos-module",
  "ownerSystem": "x86_64-linux",
  "ownerGeneration": "/nix/store/00000000000000000000000000000000-owner-new",
  "supportedSystems": [
    "x86_64-linux"
  ],
  "adapter": "/nix/store/00000000000000000000000000000000-adapter/bin/adapter",
  "guests": {
    "guest-a": {
      "system": "x86_64-linux",
      "desiredIdentity": "/nix/store/00000000000000000000000000000000-guest-a",
      "artifact": "/nix/store/00000000000000000000000000000000-guest-a",
      "runtimeInstance": "runtime-guest-a",
      "readiness": {
        "contract": "/nix/store/00000000000000000000000000000000-probe",
        "deadlineSeconds": 30
      },
      "recoveryContract": "/nix/store/00000000000000000000000000000000-restore"
    },
    "guest-b": {
      "system": "x86_64-linux",
      "desiredIdentity": "/nix/store/00000000000000000000000000000000-guest-b",
      "artifact": "/nix/store/00000000000000000000000000000000-guest-b",
      "runtimeInstance": "runtime-guest-b",
      "readiness": {
        "contract": "/nix/store/00000000000000000000000000000000-probe",
        "deadlineSeconds": 30
      },
      "recoveryContract": "/nix/store/00000000000000000000000000000000-restore"
    }
  }
}
```

The consumer authoring example is
[`tests/fixtures/vm-owner-artifacts.nix`](../tests/fixtures/vm-owner-artifacts.nix).
It constructs independent guest NixOS configurations first. Its consumer-style
owner module references their toplevel outputs directly in the JSON template;
guest configurations never reference the owner configuration. During the owner
toplevel build it substitutes the actual `$out` for `ownerGeneration`, avoiding
a recursive evaluation of `config.system.build.toplevel` inside its own manifest.
This is build-time artifact authoring, never activation-time evaluation or
construction. The fixture adapter intentionally refuses execution and does not
qualify a real runtime. `vm-owner-artifacts` evaluates this graph and rejects the
negative unrelated-guest manifest; Rust tests also reject relation/system
mismatches with typed field errors.

The opt-in helper module does not add a second manifest authoring surface:
`host.vmApplication` remains the pure compatibility facet, while the consumer's
runtime module owns this artifact. See [owner backend authoring](vm-owner-backend.md)
for the helper, adapter protocol, lock, readiness and recovery contracts.

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

This pure function performs no approval or effects. Native CLI/TUI/MCP use the
same resolved owner plan, settings, mode and inventory-content identity for
approval. CLI passes `--approve-owner-plan <requiredOwnerApproval>`; MCP passes
`owner_approval`. A missing/stale approval refuses before registration/build,
including when VM impact disappears. TUI captures the displayed plan instead of
silently refreshing approval; the native child independently revalidates it.
If the full native TUI modal cannot fit, confirmation is refused: enlarge the
terminal or inspect/approve through CLI/MCP. Owner host confirmation alone cannot
prove guest success. The optional [owner helper](vm-owner-backend.md) supplies the
serialized runtime seam, not a runtime or permission for consumer adoption.

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
Missing helper, manifest, prior runtime or consumer capability refuses rather
than falling back to a guest endpoint. Writable disks, application data and
external services are not reverted.

## Tests

`vm-owner-resolution` runs the isolated Rust owner-plan and manifest validation
suite: no owner/guest/network is contacted. It covers version negotiation,
forged aggregates, immutable dependencies, sibling exclusions, deduplication,
approval fingerprint changes, source/settings/mode/impact changes and refusal
before run allocation. The backend check also gates shared approval reconstruction,
frontend settlement, frozen VM campaigns and partial-failure owner-unit retries.
VM campaigns write schema version 2; non-VM campaigns retain version-1 bytes.
Guest identities narrow retries to their complete bound owner unit, never a
partial guest application. Campaign `required_confirm` includes the owner-plan
fingerprint; exact owner names alone are insufficient. Missing bound guest runtime
evidence is explicitly `guest evidence missing`, never inferred from an owner
survey or a successful reboot command.

`vm-member-schema` tests local/module and fleet validation, including a negative
macOS owner and missing configurations/artifacts. `vm-projections` tests direct
and integrated/controller-platform guards, canonical/publication facts, owner
capacity trust/self-exclusion, pure JSON with poisonous toplevels, owner selection
and exclusions, and exact standalone aggregate/deploy/inventory/batch bytes.
`tests/fixtures/vm-baseline` was captured from the pre-VM implementation; it is
not regenerated from expected new behavior.
