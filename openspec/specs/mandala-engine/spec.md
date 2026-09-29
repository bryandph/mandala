# mandala-engine Specification

## Purpose
The public mandala projection engine product: fleet contract schemas, a pure projection lib (depending only on `nixpkgs.lib`), exported flakeModules/nixos-modules, templates, a showcase example fleet, adoption docs, and the fleet CLI.
## Requirements
### Requirement: Layered repo with a purity invariant
The mandala engine flake SHALL declare nixpkgs as its ONLY flake input, and
its layers SHALL be gated by allowed dependencies: `schema/` modules import
nothing, `lib/` uses only `nixpkgs.lib`, `flake-modules/` and
`nixos-modules/` are exported as module PATHS (no inputs added to the
engine), `ansible/` and `cli/` are content/packages built from nixpkgs, and
only `examples/` may pin third-party flakes.

#### Scenario: lib-only consumers stay weightless
- **WHEN** a consumer pins mandala and evaluates only `lib`/`schemas`
- **THEN** no inputs beyond nixpkgs are fetched and no package derivations
  (CLI, collection) are instantiated

#### Scenario: toolchains are injected, never pinned
- **WHEN** `lib.projections.deployNodes` is called
- **THEN** deploy-rs and nixpkgs arrive as function ARGUMENTS from the
  caller, and the engine's flake.lock contains no deploy-rs entry

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

#### Scenario: porcelain reads the fleet in one eval
- **WHEN** the CLI needs members, groups, and inventory
- **THEN** one `nix eval --json .#mandala` returns a shape gated by
  `schemaVersion`, and no porcelain code scrapes per-tool outputs

### Requirement: Secret-grade secrets declarations
`schema/secrets.nix` SHALL declare named secrets with a file path, readers
(`members`, `groups`, `all`, `adminOnly`), and custody metadata, validated
at eval time: every resolved reader MUST have a sops recipient, referenced
groups MUST be non-empty, `adminOnly` MUST exclude other readers, and paths
MUST be unique. The `.sops.yaml` projection SHALL be computed from these
declarations. Declarations are AUTHORED in consumer repos; the public
engine holds only the schema and fictional example data.

#### Scenario: a reader without a recipient fails eval
- **WHEN** a declared secret's reader set resolves to a member whose
  `deployment.sops.recipient` is null
- **THEN** evaluation fails naming the secret and the member, instead of
  silently omitting the member from the creation rule

#### Scenario: admin-only secrets seal to the operator anchor alone
- **WHEN** a secret is declared `adminOnly`
- **THEN** its generated creation rule carries the operator PGP anchor and
  NO age recipients

### Requirement: Documented adoption surface
The engine SHALL ship `docs/convention.md` stating exactly what a consumer
flake must expose (member-schema'd `nixosConfigurations`, optional
`data.members`, expected input names for injected toolchains), a
`templates/fleet` flake template, and an `examples/showcase` fictional
fleet exercising every projection. Docs SHALL state which projections the
showcase asserts versus only illustrates.

#### Scenario: a stranger adopts mandala from the template
- **WHEN** a user runs `nix flake init -t github:bryandph/mandala#fleet`
  and fills in one member
- **THEN** the resulting flake evaluates a projected ansible inventory and
  deploy node without reading mandala source code

#### Scenario: the showcase cannot rot silently
- **WHEN** engine CI runs
- **THEN** the showcase example evaluates as a check alongside fake-fleet

### Requirement: Topology values are semantically validated
The topology schema SHALL reject VLAN identifiers outside the IEEE 802.1Q usable range while retaining `0` as the existing explicit non-VLAN/overlay sentinel, and SHALL validate authored IPv4/IPv6 address prefixes, gateways, DNS addresses, and ULA prefixes for syntax and numeric bounds before projections consume them.

#### Scenario: an invalid VLAN or prefix fails evaluation
- **WHEN** topology data contains an out-of-range VLAN identifier, malformed address, or invalid prefix length
- **THEN** schema evaluation fails naming the invalid topology field

### Requirement: PKI signer relationships are validated
The PKI schema SHALL require roots to have no signer, intermediates to name an existing CA, and the signer graph to be acyclic. An intermediate without a signer, a root with a signer, an unknown signer, or a signing cycle MUST fail evaluation.

#### Scenario: an unsigned intermediate fails evaluation
- **WHEN** a CA with role `intermediate` has no `signedBy` value
- **THEN** schema evaluation fails naming that CA

#### Scenario: a signer cycle fails evaluation
- **WHEN** following `signedBy` relationships returns to an already visited CA
- **THEN** schema evaluation fails and identifies the cycle
