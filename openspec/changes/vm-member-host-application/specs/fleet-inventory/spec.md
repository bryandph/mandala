## MODIFIED Requirements

### Requirement: Class-agnostic, management-tier-aware members
The system SHALL model members as class-agnostic with a set of management surfaces, so a standalone member with no management surface is a facts-only entry that appears in DNS/docs and that nothing pushes to.

A VM with a valid typed owner relation SHALL be owner-managed despite having its SSH management surfaces disabled. Only standalone members without management remain facts-only; disabled guest SSH surfaces MUST NOT cause a valid VM to be treated as undeployable through its owner.

#### Scenario: an unmanaged switch is facts-only
- **WHEN** a network switch is declared with an empty `manage` set
- **THEN** it appears in generated DNS/docs but produces no deploy, ansible, or push target

#### Scenario: owner-managed VM has no guest SSH surface
- **WHEN** a VM has a supported directly managed owner and both guest SSH management enables are false
- **THEN** it remains a first-class member eligible for owner-unit resolution, not an independent push target or facts-only lifecycle entry

### Requirement: Unified group derivation
The system SHALL derive deploy `@group`, ansible `-l group`, and sops recipient-group membership from one `mandala.lib` group function, so canonical member taxonomy cannot drift. Transport eligibility SHALL be applied after taxonomy: VM identities remain in canonical groups and applicable sops reader groups but SHALL be absent from SSH Ansible groups and raw deploy-batch entries. Native deploy selectors SHALL expand selected VMs to owner units with explicit impact preview; raw tool projections SHALL NOT silently perform this expansion.

#### Scenario: an ansible-only group is reachable by deploy too
- **WHEN** a host declares an extra group via `host.deployment.ansible.groups` (e.g. `web_edge`)
- **THEN** that group has the same canonical member set for deploy and Ansible derivation, with only transport-eligible standalone members emitted as SSH targets

#### Scenario: mixed group preserves identity without creating guest targets
- **WHEN** a canonical group includes a VM and a standalone owner
- **THEN** native selection retains both identities and resolves one owner unit, while Ansible SSH inventory and the raw deploy batch contain no independent VM entry

### Requirement: Aggregate output for porcelain consumers
A consuming flake SHALL expose the engine's versioned aggregate output
(`flake.mandala`) so fleet porcelain (capability `fleet-cli`) and operator
engines resolve members, groups, and projections from one eval surface.

#### Scenario: CLI group resolution matches fan-out surfaces
- **WHEN** porcelain resolves `@web` via the aggregate output
- **THEN** the canonical member set is shared with tool projection derivation;
  standalone SSH-eligible fleets retain equal deployBatch/Ansible membership,
  while VM-bearing groups apply transport guards and native owner expansion explicitly

## ADDED Requirements

### Requirement: Typed VM owner relation
Members SHALL expose `host.kind` as `standalone | vm`, default `standalone`, and nullable typed `host.vm`. VM kind SHALL require exactly one `vm.owner` canonical member ID and `vm.backend`; standalone kind SHALL forbid a non-null VM relation. The initial backend enum SHALL contain only `nixos-module`. Category SHALL remain organizational; platform SHALL remain OS/venue; build SHALL remain closure-construction metadata. None SHALL infer ownership.

#### Scenario: a module-managed NixOS guest is declared
- **WHEN** acme declares a NixOS VM with owner `host-a`, backend `nixos-module` and its own Linux build system
- **THEN** the guest retains its own member identity, platform, build and network facts, distinct from the owner

#### Scenario: a legacy category does not opt in
- **WHEN** a member sets `category = "vm"` without a kind or relation
- **THEN** it retains default standalone semantics rather than acquiring an inferred owner

#### Scenario: kind and relation disagree
- **WHEN** a VM has no relation or a standalone member has a non-null relation
- **THEN** every member schema entry point rejects the conflicting shape

### Requirement: Fleet-level ownership validation and derived guest index
After collision-free member assembly, the fleet SHALL validate that every VM owner exists, differs from the guest, is standalone and directly managed, and supports the declared backend/guest pair. It SHALL reject cycles, nested hosting, multiple authoritative owners and unavailable backend artifacts. For `nixos-module`, owner and guest SHALL have NixOS platform and consumer-supplied configurations, the owner SHALL enable deploy-rs and expose the backend manifest, and the guest SHALL have build metadata. Runtime support for the guest system SHALL be explicit. `projections.vm.guestsByOwner` SHALL be a sorted owner-to-guest-ID index derived only from guest relations.

#### Scenario: invalid ownership fails before effects
- **WHEN** a VM references a missing, self, VM, unmanaged or unsupported owner, or a relation cycle is supplied
- **THEN** fleet validation fails with the involved IDs before selection can start build or transport effects

#### Scenario: a guest system is unsupported
- **WHEN** an owner module does not declare support for the guest build system
- **THEN** validation refuses the backend pair even if both members declare NixOS platform

#### Scenario: multiple guests share one owner
- **WHEN** two VM relations name the same standalone owner
- **THEN** the derived index contains both guest IDs in sorted order without any authored reverse list

#### Scenario: macOS application is not implied
- **WHEN** a Linux VM names a macOS owner without an approved host-application adapter
- **THEN** validation refuses application support; NixOS deploy-node behavior is not generalized to macOS

### Requirement: VM publication and transport eligibility are distinct
VMs SHALL remain in aggregate inventory, canonical taxonomy and documentation, and in DNS/DHCP projections when their own real address facts qualify. VMs SHALL be absent from Ansible SSH inventory, deploy-rs nodes, independent deploy-batch entries and builder routes. The fleet SHALL reject any effective true VM `deployment.ansible.enable` or `deployment.deployRs.enable`, including inherited factory defaults. Projection guards SHALL still exclude VMs independently of validation, hooks, direct entry points and controller-platform variants.

#### Scenario: a guest publishes its own network facts
- **WHEN** a VM has a qualifying DNS-role address
- **THEN** its own forward/reverse records and ownership documentation appear without substituting the owner's endpoint

#### Scenario: a factory accidentally enables guest SSH
- **WHEN** an effective guest Ansible or deploy-rs enable is true
- **THEN** evaluation rejects the contradiction naming guest and owner, rather than silently targeting either endpoint

#### Scenario: a direct projection sees contradictory data
- **WHEN** a direct library projection receives VM data with an SSH enable or an injection hook attempts to add the guest
- **THEN** it never emits a guest SSH host, deploy-rs node or independent batch entry

### Requirement: Guest capacity is advertised only by the owner
A VM SHALL NOT advertise a builder route; an enabled VM builder facet SHALL fail validation. Guest `build.system/buildPlatform` SHALL describe construction, not execution capacity. Guest-backed capacity SHALL be explicitly advertised only on the owner's `host.builder`, with systems/features/lanes and strict owner-endpoint trust, and the owner daemon SHALL actually dispatch that work. Neither duplicate guest advertisements nor inferred emulation SHALL be introduced.

#### Scenario: an owner provides Linux guest-backed capacity
- **WHEN** the owner explicitly advertises supported Linux capacity and its daemon dispatches it locally
- **THEN** controller builder routes target the owner only, while owner self-exclusion leaves that capacity local

#### Scenario: VM builder advertisement is rejected
- **WHEN** a VM enables a builder facet or a malformed direct projection input attempts a VM route
- **THEN** validation rejects the declaration and no projection emits a guest builder route
