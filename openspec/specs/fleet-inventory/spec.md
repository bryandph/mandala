# fleet-inventory Specification

## Purpose
The fleet contract: members authored by the fleet's own configurations and
operator data, validated against mandala's schemas, and projected outward
(ansible inventory, deploy nodes and batches, sops rules, DNS/DHCP and mesh
records) without mandala ever generating host configurations. How a specific
operator composes this contract is specified in that operator's repository.

## Requirements
### Requirement: Engine/data split
The system SHALL separate the fleet contract into a generic public engine (`mandala`: schema + projection lib, depending only on `nixpkgs.lib` at the library layer) and operator data (the values filling mandala's schema), owned by the consuming operator. Operator data SHALL be validated against mandala's schema at eval time.

#### Scenario: engine has no operator leakage
- **WHEN** mandala is evaluated against its bundled fake-fleet example
- **THEN** it produces projections without referencing any real organization's network, key, or address

#### Scenario: operator data is validated at eval time
- **WHEN** operator data is evaluated with member/topology/operator values
- **THEN** those values are checked against mandala's schema (via `evalModules`) and an invalid entry fails the build

### Requirement: Configs author inventory, projections flow outward
The system SHALL treat `nixosConfigurations` as the source of truth: NixOS members author their entry in-config as `host = {…}`. The consumer's flake SHALL project the merged member view (NixOS configs ∪ operator-declared non-NixOS members) into ansible inventory, deploy nodes/groups, sops `.sops.yaml`, DNS/DHCP records, and mesh members.

#### Scenario: a NixOS host change propagates to projections
- **WHEN** a NixOS host's in-config `host` entry changes
- **THEN** the regenerated ansible inventory, deploy nodes, and address records reflect it without editing those authorities by hand

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

### Requirement: Outward publication projections
The system SHALL expose `dnsRecords` (A/AAAA + PTR), `dhcpReservations`, and mesh member records derived from the merged member view, with DNS names derived (`${name}.${planeSuffix}.${zone}`) rather than authored. Consumers render these into their DNS/DHCP authorities rather than hand-authoring the records.

#### Scenario: a member's DNS name and records are derived
- **WHEN** a member with a `dns`-role address is projected
- **THEN** its forward record and matching PTR appear in `dnsRecords` under the derived name, and no hand-authored copy of that record is needed

#### Scenario: generated records are reconciled against the live authority before flip
- **WHEN** a consumer generates `dnsRecords`/`dhcpReservations` for the first time
- **THEN** they are diffed against the live DNS/DHCP authority and confirmed to match (or be a reviewed superset) before any record is cut over

### Requirement: Projections are engine-implemented
Ansible inventory, sops config, deploy nodes, and deploy batch SHALL come
from `mandala.lib.projections` / the engine's flakeModules. A consumer's
modules SHALL contain only imports, hook values, and operator credentials,
with no projection logic. Moving a projection from a consumer into the
engine MUST be gated by byte-identical live projection output
(`nix eval --json` of the projection) before and after.

#### Scenario: parity gate protects a lift step
- **WHEN** a projection's implementation moves from a consumer module to
  the engine
- **THEN** the captured before/after JSON of that projection's live output
  is identical, hooks included

### Requirement: Secrets routing is validated contract data
Sops routing SHALL be expressed as secret-grade declarations validated
against the engine's secrets schema (see capability `mandala-engine`), and
the generated `.sops.yaml` SHALL derive from those declarations. A consumer
replacing a hand-authored routing table MUST verify recipient-set parity
with the previous generation at switch time.

#### Scenario: routing parity at the swap
- **WHEN** the declarations replace a hand-authored routing table
- **THEN** the generated creation rules' recipient SETS are identical to
  the prior output (zero `sops updatekeys` churn), and the silent-drop
  case (reader without recipient) now fails eval instead

### Requirement: Aggregate output for porcelain consumers
A consuming flake SHALL expose the engine's versioned aggregate output
(`flake.mandala`) so fleet porcelain (capability `fleet-cli`) and operator
engines resolve members, groups, and projections from one eval surface.

#### Scenario: CLI group resolution matches fan-out surfaces
- **WHEN** porcelain resolves `@web` via the aggregate output
- **THEN** the canonical member set is shared with tool projection derivation;
  standalone SSH-eligible fleets retain equal deployBatch/Ansible membership,
  while VM-bearing groups apply transport guards and native owner expansion explicitly

### Requirement: Canonical member identifiers
Every member map key and `host.name` SHALL be one bare RFC 1123 host label: 1–63 ASCII letters, digits, or hyphens, beginning and ending with a letter or digit. It SHALL contain no dot or domain suffix, SHALL not be a reserved selector token, and SHALL be equal to its member map key. Network-plane and domain data SHALL remain separate inputs used by Mandala's FQDN projection. Rust inventory loading and Nix fleet assembly MUST reject invalid or mismatched identifiers before selector resolution, Nix attribute construction, or filesystem writes.

#### Scenario: an unsafe identifier is rejected
- **WHEN** a member key or name is empty, longer than 63 characters, starts or ends with a hyphen, contains a dot, underscore, path separator, selector delimiter, whitespace, or reserved token
- **THEN** inventory evaluation/loading fails with the offending identifier and the RFC 1123 bare-label requirement

#### Scenario: FQDN construction remains projection-owned
- **WHEN** a valid bare member hostname is projected onto a network plane and domain
- **THEN** Mandala constructs the FQDN from those separate values rather than requiring or accepting an FQDN as the member selector

#### Scenario: a key and declared name disagree
- **WHEN** a member map key differs from its `host.name`
- **THEN** fleet assembly fails instead of exposing two identities for the member

### Requirement: Member sources are collision-free
The fleet module SHALL require the NixOS-derived member keys and `extraMembers` keys to be disjoint and MUST fail evaluation naming every collision before merging the sources.

#### Scenario: an extra member collides with a NixOS member
- **WHEN** `extraMembers` declares a key already emitted by `nixosConfigurations`
- **THEN** evaluation fails naming the duplicate rather than allowing either record to override the other

### Requirement: Member invariants apply at every schema entry point
Member cross-field invariants SHALL be defined once and enforced for direct Nix module consumers and `evalMember` consumers. These invariants MUST include unique network roles and the dependency of reservation data on a usable address.

#### Scenario: duplicate DNS roles fail direct module evaluation
- **WHEN** a NixOS `host` module declares more than one network with the DNS role
- **THEN** module evaluation fails with the same invariant that rejects the value through `evalMember`

#### Scenario: reservation without an address fails consistently
- **WHEN** a member requests a DHCP reservation on a network without a usable address
- **THEN** both direct module evaluation and `evalMember` reject the member

### Requirement: Layered deploy settings
The fleet contract SHALL support deploy settings at three tiers — a fleet
default, a per-group tier keyed by the sanitized taxonomy spelling, and the
member's own declarations — merged deterministically with member > group >
fleet-default precedence for scalar settings and member-first append for
`sshOpts`. Within the group tier, a member's groups SHALL merge in sorted
(lexicographic) name order with the later group overriding the earlier — a
documented, deterministic rule; the member tier is the explicit override.
Group-settings keys referencing unknown groups SHALL fail eval. The merge
SHALL be implemented once in the engine lib and flow into every projection
— no consumer re-merges. `autoRollback` and `fastConnection` SHALL use
nullable authoring values at every tier; when no tier authors either value,
the flattened result SHALL supply the legacy effective default `true` so an
unchanged fleet keeps its existing deploy-node behavior.
The connection endpoint (`hostname`), SSH login user, SSH port,
identity-file path, target profile user, and sudo command SHALL be scalar
settings in the same merge. The effective endpoint SHALL fall back to the
member's primary FQDN, and may be overridden by an IP or alternate hostname
at any tier. Login user and port SHALL retain legacy effective fallbacks
(`root` and `22`) only after tier resolution. Identity files SHALL be
represented only by path strings; their contents SHALL never enter a
projection or store derivation.

#### Scenario: a group tier applies to its members
- **WHEN** `deployment.groupSettings.web` sets `confirmTimeout` and a web
  member authors nothing
- **THEN** that member's flattened settings carry the group value while
  non-web members keep the fleet default

#### Scenario: sibling groups resolve by sorted order
- **WHEN** two groups sharing a member set the same scalar to different
  values and the member does not override it
- **THEN** the lexicographically later group's value wins, identically on
  every evaluation

#### Scenario: the member tier wins
- **WHEN** a member authors a setting also set by its group and the fleet
  default
- **THEN** the flattened result carries the member's value

#### Scenario: primary FQDN is the default endpoint
- **WHEN** no tier authors a connection endpoint for a member
- **THEN** every deploy consumer connects to that member's primary FQDN

#### Scenario: a group overrides connection identity
- **WHEN** a group authors an alternate endpoint, non-root SSH login user,
  identity-file path, target user, and sudo command
- **THEN** native deployment, raw deploy-rs, and Ansible inventory receive
  those identical effective values unless the member overrides them

### Requirement: Fleet-wide selector alias and enumerating group errors
Selector resolution SHALL treat `@all` as an alias for the reserved
fleet-wide keyword `all`, resolving to every member; `all` SHALL remain
reserved as both a member name and a group name so the alias can never be
shadowed. Resolution failure on an unknown group SHALL produce an error
that names the unknown group and enumerates the available group names in
sorted order.

#### Scenario: both fleet-wide spellings resolve identically
- **WHEN** `@all` and `all` are each resolved against the same inventory
- **THEN** both return the identical sorted full member set, and exclusions
  compose with either spelling (`@all,!x` equals `all,!x`)

#### Scenario: a group named "all" cannot exist
- **WHEN** an aggregate attempts to define a group literally named `all`
- **THEN** inventory loading rejects it as a reserved selector token rather
  than allowing it to shadow the fleet-wide alias

#### Scenario: unknown-group errors enumerate the taxonomy
- **WHEN** a selector references a group absent from the inventory
- **THEN** the error message names the missing group and lists the available
  group names

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
