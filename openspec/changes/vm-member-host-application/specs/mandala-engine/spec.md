## MODIFIED Requirements

### Requirement: Engine-side projections with consumer hooks
`mandala.lib.projections` SHALL implement the fleet projections (ansible
inventory, sops config, deploy nodes, deploy batch) as pure functions over
validated members, with consumer-specific values supplied through explicit
hook arguments (e.g. `extraHostvars`) — never hardcoded. NixOS conventions
(the `ansible_python_interpreter` system-profile pin for NixOS members —
`platform == "nixos"` or a consumer-built closure (`host.build != null`),
since cloud members author their hosting venue as `platform` — and the
synthetic `deploy_rs` guard group) SHALL be emitted by default and
overridable. The deploy-nodes projection SHALL emit each node's FLATTENED
merged deploy settings (full deploy-rs vocabulary plus the lowered endpoint,
SSH login, port, and identity-file settings from the layered tiers), so the
raw deploy CLI, deployChecks, and the native engine act on identical per-node
truth. The Ansible inventory projection SHALL consume the same flattened
connection and privilege result for its standard SSH hostvars plus explicit
deploy target-user/sudo hostvars; it SHALL NOT independently merge deploy
tiers. Backend lowering of an authored identity SHALL include both
`IdentitiesOnly=yes` and `IdentityAgent=none` wherever the consumer supports
OpenSSH options.

VM transport eligibility SHALL be an invariant before and after consumer hooks: no Ansible SSH inventory, deploy-rs node, independent batch entry or builder route may target a VM. Direct library calls, flake outputs and controller-platform variants SHALL obey the same guard; hook overrides SHALL NOT bypass it. Owner application artifacts MAY reference guest closures transitively without emitting guest nodes.

#### Scenario: a hook injects repo-specific hostvars
- **WHEN** a consumer passes `extraHostvars = name: {...}` to the ansible
  inventory projection
- **THEN** the returned inventory merges those vars per host after the
  engine defaults, so hooks can also override convention values

#### Scenario: cross-compiled members deploy correctly
- **WHEN** a member's `host.build.buildPlatform` differs from its target
  system
- **THEN** the deploy-nodes projection produces an activate profile whose
  scripts execute on the TARGET platform (host-platform shebangs,
  build-platform fallback for deploy-rs lib selection)

#### Scenario: the escape hatch sees the merged truth
- **WHEN** the operator runs the raw deploy CLI against a projected node
- **THEN** the node's settings (timeouts, rollback flags, ssh options) are
  the same flattened values the native engine executes with

#### Scenario: Ansible projects the same connection contract
- **WHEN** a fleet, group, or member overrides endpoint, SSH login user,
  port, identity file, target profile user, or sudo command
- **THEN** the Ansible hostvars and native/deploy-rs settings represent the
  same resolved values

### Requirement: Versioned aggregate contract output
The fleet flakeModule SHALL emit a single aggregate output (`flake.mandala`)
containing a `schemaVersion` plus the merged member view, group taxonomy,
and projection results, as the one eval surface for porcelain (CLI/TUI and
plugged engines). Per-tool outputs (ansible inventory, sops config, deploy
nodes/batch) SHALL remain available for direct tool consumption.

A VM-bearing fleet SHALL use aggregate schemaVersion 2, carrying typed ownership, a sorted derived guestsByOwner index and owner application installable-reference strings as pure data. No derivation or realization SHALL enter JSON evaluation. VM-aware readers SHALL accept supported versions 1 and 2; unsupported versions SHALL be refused before mutation. Non-VM fleets MAY preserve the version-1 shape and bytes. Raw tool outputs SHALL remain filtered transport views, not implicit owner-selection expansion.

#### Scenario: porcelain reads the fleet in one eval
- **WHEN** the CLI needs members, groups, and inventory
- **THEN** one `nix eval --json .#mandala` returns a shape gated by
  `schemaVersion`, and no porcelain code scrapes per-tool outputs

#### Scenario: pure VM aggregate
- **WHEN** a VM-bearing fleet is evaluated as JSON
- **THEN** ownership and owner application references are available in version 2 without realizing guest or owner derivations

#### Scenario: old deployment reader encounters ownership
- **WHEN** a reader supporting only version 1 encounters a VM-bearing version-2 aggregate
- **THEN** it rejects the contract before selecting or mutating a target

#### Scenario: existing fleet remains unchanged
- **WHEN** a fleet has no typed VM relations and retains version 1
- **THEN** its inventory/settings/tool projection bytes and direct standalone deployment behavior remain unchanged

## ADDED Requirements

### Requirement: Host-application backend dependencies are consumer-injected
The engine SHALL define the initial `nixos-module` backend contract without pinning a virtualization toolchain or generating guest systems. Consumer configurations SHALL supply the module/runtime adapter and owner artifacts referencing guest outputs in one fleet graph. Pure projection validation SHALL reject manifest/ownership mismatches and unsupported owner/backend/system combinations before effects.

#### Scenario: manifest names an unrelated guest
- **WHEN** an owner artifact manifest lists a guest owned by a different member
- **THEN** validation rejects the artifact instead of allowing cross-owner application

#### Scenario: toolchain ownership stays with the consumer
- **WHEN** the consumer supplies its NixOS runtime module and guest configurations
- **THEN** Mandala projects their validated references without adding a virtualization flake input or creating a second configuration authority
