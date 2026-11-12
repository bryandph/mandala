## MODIFIED Requirements

### Requirement: Typed VM owner relation
Members SHALL expose `host.kind` as `standalone | vm`, default `standalone`, and nullable typed `host.vm`. VM kind SHALL require exactly one `vm.owner` canonical member ID and `vm.backend`; standalone kind SHALL forbid a non-null VM relation. Supported backend values SHALL be `nixos-module` and `darwin-module`, each restricted to its qualified owner/guest pair. Category SHALL remain organizational, platform SHALL remain OS/venue, and build SHALL remain closure-construction metadata; none SHALL infer ownership.

#### Scenario: Darwin owner relation is explicit
- **WHEN** acme declares a NixOS Linux VM with backend `darwin-module` and owner `workstation-a`
- **THEN** the guest retains its own member identity and Linux build system while its owner retains Darwin platform

#### Scenario: legacy category is unchanged
- **WHEN** an existing member sets category vm without a kind or relation
- **THEN** no owner is inferred and standalone semantics remain unchanged

#### Scenario: conflicting local shape
- **WHEN** a VM omits its relation or a standalone member supplies one
- **THEN** all schema entry points reject the declaration

### Requirement: Fleet-level ownership validation and derived guest index
After collision-free member assembly, the fleet SHALL validate that every VM owner exists, differs from the guest, is standalone and directly managed, and supports the backend/guest pair. It SHALL reject cycles, nested hosting, multiple authoritative owners and unavailable artifacts. For `nixos-module`, owner and guest SHALL have consumer-supplied NixOS configurations and NixOS platform. For `darwin-module`, the owner SHALL have an enrolled consumer-supplied Darwin configuration, Darwin platform and the `darwin-system` application adapter; the guest SHALL have a consumer-supplied NixOS configuration, NixOS platform and explicit Linux build system. Both backends SHALL require owner deploy-rs enablement, build metadata, a matching typed manifest and explicit runtime system support. The sorted `projections.vm.guestsByOwner` index SHALL be derived only from guest relations.

#### Scenario: supported Darwin pair
- **WHEN** an enrolled managed Darwin owner provides a matching manifest and explicit support for its Linux guest system
- **THEN** fleet validation admits the relation and derives the owner-to-guest index

#### Scenario: facts-only Mac is not a deploy adapter
- **WHEN** a guest references a Darwin extra member with no enrolled owner configuration
- **THEN** validation refuses before effects regardless of the member's builder capacity

#### Scenario: unsupported relation
- **WHEN** ownership is missing, self-referential, cyclic, nested or incompatible with the backend/system pair
- **THEN** validation rejects the involved IDs before build or transport

#### Scenario: NixOS support is preserved
- **WHEN** a valid nixos-module pair is evaluated
- **THEN** its existing pair validation and sorted derived index are unchanged

## ADDED Requirements

### Requirement: Explicit Darwin owner enrollment
The fleet SHALL discover validated `config.host` members from consumer `darwinConfigurations` additively beside NixOS configurations and extra members. Namespaces SHALL be collision-free with matching canonical configuration/member IDs. A directly managed Darwin owner SHALL explicitly enable deploy-rs, use a compatible Darwin build system and root system-profile activation, and supply its application artifact and backend capability. A platform tag, builder facet or facts-only extra member SHALL NOT opt into deployment.

#### Scenario: facts-only member is enrolled deliberately
- **WHEN** operator-alice removes a facts-only extra member and provides the same ID as an enabled Darwin configuration with a compatible owner artifact
- **THEN** the ID becomes directly managed in the same fleet graph without a second membership authority

#### Scenario: duplicate ID
- **WHEN** a Darwin configuration ID also occurs in NixOS configurations or extra members
- **THEN** assembly rejects the collision even if the values appear equal

#### Scenario: unenrolled members retain behavior
- **WHEN** a fleet has only existing NixOS configurations and facts-only Darwin extra members
- **THEN** no new Darwin application unit or transport route is inferred
