# builder-capacity Specification

## Purpose
Optional member builder capacity, explicit controller profiles, deterministic lane and route projections, and opt-in platform adapters with strict system-wide SSH identity and host trust.

## Requirements
### Requirement: Builder capability is an optional member facet

The system SHALL support `host.builder` as null (the default, offering no capacity)
or a validated capacity record, independent of closure instructions in
`host.build`, deployment settings, and operator identity. The record SHALL contain
a nonempty explicit systems set, supportedFeatures and mandatoryFeatures sets
(default empty), positive-integer maxJobs and speedFactor (default 1), a named
lanes map (default empty), and connection defaults. Mandatory features SHALL be
a subset of supported features. Systems/features SHALL normalize to sorted unique
lists. No architecture, platform or closure build field SHALL imply execution
systems, emulation, kvm, or test features.

Connection defaults SHALL support hostname, explicit builder login, port (default
22, integer 1–65535), protocol (initially ssh-ng only), optional absolute-string
remote-program path, and public host trust. Hostname MAY fall back to an available
primary member FQDN; builder login SHALL NOT inherit deployment identity.
Unknown structural fields and invalid values SHALL fail evaluation with field
context. Shared invariants SHALL apply through both direct member modules and
`evalMember`.

#### Scenario: No capacity is declared
- **WHEN** a member leaves host.builder absent or null
- **THEN** it offers no builder lanes and existing closure/deploy semantics remain unchanged

#### Scenario: Capacity is explicit rather than inferred
- **WHEN** a member declares only a closure build system or host architecture
- **THEN** the system does not advertise that system or any emulation capability as builder capacity

#### Scenario: Invalid capacity fails at both entry points
- **WHEN** a builder declares empty systems, nonpositive jobs/speed, an unsupported protocol, invalid port, unknown field, or mandatory features absent from supported features
- **THEN** direct member module evaluation and evalMember both reject it naming the invalid field

### Requirement: Facts-only builders reuse member enrollment

Builders without managed closures SHALL enroll through the existing
`extraMembers` / `evalMember` contract, with host.build null where no closure is
managed and deployment management surfaces disabled. A builder facet SHALL NOT
enable deployment/Ansible management, require a fabricated network attachment,
or bypass member source collision checks. Such members remain visible to normal
all-member selectors and inventory projections.

#### Scenario: An external builder has no managed closure
- **WHEN** acme enrolls a Linux external builder as a facts-only extra member with explicit builder endpoint data
- **THEN** it can offer build capacity without producing a deployment/Ansible push target or requiring an invented network attachment

#### Scenario: Enrollment cannot shadow a member
- **WHEN** a facts-only builder key already exists among configuration-derived members
- **THEN** fleet evaluation rejects the collision rather than replacing either record

### Requirement: Effective lanes replace implicit capacity

No lanes or an empty lane map SHALL yield one effective lane named default.
A nonempty lane map SHALL replace that implicit lane. Named lanes SHALL support
nullable overrides of systems, supportedFeatures, mandatoryFeatures, maxJobs and
speedFactor; null SHALL inherit, while authored scalar/list values SHALL replace,
not append to, the facet value. Each fully resolved lane SHALL satisfy the facet's
capability invariants. Lane names SHALL be nonempty safe labels, and systems and
feature sets SHALL normalize deterministically.

Distinct member/lane identities SHALL remain distinct even when sharing an
endpoint. Limits SHALL be documented as advertised scheduling limits, not
exclusive resources, global budgets or reservations. The system SHALL NOT claim
that one lane's maxJobs constrains another lane or controller.

#### Scenario: A default lane is implicit
- **WHEN** a builder has no named lanes
- **THEN** exactly one default lane uses its capability values

#### Scenario: Named lanes replace and clear values
- **WHEN** a builder defines wide and serial lanes, wide sets supportedFeatures to an empty list, and serial replaces systems
- **THEN** only wide and serial exist, wide does not inherit supported features, and serial uses only its authored systems

#### Scenario: A resolved lane is inconsistent
- **WHEN** a lane removes a supported feature while inheriting that feature as mandatory
- **THEN** evaluation fails naming the member and lane instead of silently unioning the feature lists

#### Scenario: Shared endpoint is not reserved capacity
- **WHEN** wide and serial lanes share one builder and another controller can use it
- **THEN** the output preserves both lanes without claiming exclusive memory or a fleet-wide job budget

### Requirement: Controllers select explicit members and lanes

The system SHALL accept `mandala.build.controllers.<id>`, defaulting to an empty
map. Each profile SHALL require an existing selfMember and an explicit
duplicate-free allowedBuilders list, and MAY provide a member-keyed lanes selection
map and routes. Every allowed builder SHALL exist and have a non-null builder
facet. Lane selection keys SHALL belong to the allowlist; referenced lanes SHALL
exist after normalization. An omitted lane selection SHALL select all effective
lanes for that allowed member; an explicit empty list SHALL select none.
Duplicate lane references SHALL fail validation.

Profiles SHALL be selected explicitly. Unknown requested profiles SHALL fail,
not fall back. Selection SHALL NOT infer from controller OS, hostname, evaluation
platform, environment, HOME, or reachability. Every lane of selfMember SHALL be
excluded by member identity, without excluding a separate external builder
member reached through it.
Authored member/lane/route references SHALL be validated even if self-excluded.

#### Scenario: Same OS does not mean same policy
- **WHEN** two Linux controller profiles declare different allowlists and identities
- **THEN** each output uses only its own selected builders and routes without union or OS fallback

#### Scenario: Self exclusion retains an external builder
- **WHEN** a macOS controller allowlists itself and its separately enrolled external builder
- **THEN** every self lane is absent and the selected builder lanes remain

#### Scenario: Bad references fail explicitly
- **WHEN** a profile has an unknown selfMember, unknown or non-builder allowlist member, duplicate member/lane, or a lane selection outside the allowlist or effective lanes
- **THEN** evaluation fails naming the profile and offending reference

#### Scenario: Empty is not fleet-wide
- **WHEN** a valid controller declares an empty allowlist or an empty lane selection
- **THEN** it selects no builders or no lanes for that member respectively, rather than falling back to all members/lanes

### Requirement: Structured routes have strict identity and host trust

Controller routes SHALL be keyed by existing member IDs and SHALL override member
connection defaults fieldwise with hostname, sshUser, port, protocol, identityFile,
optional remoteProgram, optional jumpMember, and optional trust. Authored trust
SHALL replace the entire inherited trust declaration. Selected builders SHALL
have routes; jump-only routes MAY refer to non-builder members. Every effective
selected or jump route SHALL have an endpoint, explicit login, valid port and
protocol, absolute-string identityFile and public trust.

Identity paths SHALL reject Nix path values, relative paths, environment/HOME
expansion and private material. Evaluation SHALL NOT read or check the existence
of key files. A remoteProgram, when present, SHALL be an absolute string.
Trust SHALL be either an explicit SSH public-key text pin or an explicit public
host CA with an authored matching host name/principal. Missing, empty or malformed
trust SHALL fail. All jumpMember references SHALL name routes in the same profile;
the entire authored route graph SHALL be acyclic, including self-loop rejection.
Jump endpoints and final endpoints SHALL each have validated trust.

Only typed connection data SHALL be accepted, not arbitrary SSH directives or
query strings. Unsafe whitespace/control characters and field/directive/URI
delimiter injection SHALL fail validation; legitimate typed remote-program data
SHALL be URI-encoded exactly once. Profile/lane identifiers and generated aliases
SHALL be safe and collision-free. No route SHALL request interactive acceptance,
runtime keyscan, StrictHostKeyChecking=no, SSH-agent signing or invoking-user HOME
discovery.

#### Scenario: A builder is reached differently by two controllers
- **WHEN** a Linux controller under WSL routes a builder through a macOS jump route and the macOS controller routes it directly on a nondefault port
- **THEN** each profile preserves its authored route and identity, pins both jump and final hosts where used, and identifies the builder separately from the controller

#### Scenario: Route overrides replace defaults
- **WHEN** a builder has default endpoint/trust and a profile authors another endpoint and a complete replacement trust declaration
- **THEN** effective values use the authored endpoint and only the replacement trust, leaving unrelated defaults intact

#### Scenario: Key or trust data is unsafe or absent
- **WHEN** a selected or jump route lacks key/trust, supplies a Nix path or relative identity, or contains an injected newline/directive/query
- **THEN** evaluation fails with profile, route and field context without reading private key contents

#### Scenario: A jump graph is invalid
- **WHEN** any authored route references an absent route, itself, or a cycle through other routes
- **THEN** evaluation fails before projection, including for otherwise unselected routes

### Requirement: Pure per-controller outputs share one canonical model

The system SHALL expose `buildMachinesByController.<id>` as a list of ordinary
nix.buildMachines-shaped attrsets and aggregate
`projections.builders.byController.<id>` as
`{ buildMachines; ssh; knownHosts; }` from one canonical normalized model.
The direct and aggregate machine lists SHALL be identical.

Machine entries SHALL carry hostName (generated SSH alias), protocol, sshUser,
sshKey (absolute string), systems, maxJobs, speedFactor, supportedFeatures,
mandatoryFeatures, and publicHostKey (encoded pin where applicable, null for
explicit CA trust). Remote-program data SHALL be encoded into the supported
machine connection representation rather than added as an unknown machine field.
SSH data SHALL preserve typed endpoints, ports, logins, identity paths,
remote-program data, jump aliases, host-key aliases, and strict host/identity
policy (strict checking, IdentitiesOnly and IdentityAgent none). knownHosts data
SHALL preserve public pins or CA trust and the names/ports/principals they match.
A nondefault-port builder SHALL have a distinct stable host-key identity.
Nix publicHostKey pin encoding SHALL base64-encode the complete SSH public-key
text, not reuse only its existing base64 key blob.

Order SHALL be lexicographic member then lane, independent of authoring order;
different lanes sharing an endpoint SHALL NOT be deduplicated. Generated alias
identity SHALL be stable and collision-free across controller/member/lane tuples.
All aggregate data SHALL be JSON-compatible primitives/attrs/lists; no
derivations, Nix paths, modules or private key material SHALL appear. Projection
SHALL be pure, with no runtime discovery, key-file reads or network access.
Structured SSH/knownHosts output SHALL describe intended system daemon
configuration, not claim it is activated. This slice SHALL NOT install adapters
or render machines/SSH files.

#### Scenario: Lists preserve lane identity
- **WHEN** wide and serial lanes use the same endpoint with different scheduling features and limits
- **THEN** the machine list retains two entries with stable lane identities and their distinct values

#### Scenario: Deterministic serializable projection
- **WHEN** equivalent fake fleets vary attribute insertion order and systems/features list order
- **THEN** fully forced JSON projection bytes are identical and each direct list equals its aggregate counterpart

#### Scenario: Trust encodings and ports are preserved
- **WHEN** synthetic fixtures use a full SSH public-key pin, a CA, and a nondefault-port builder
- **THEN** machine pin encoding, public knownHosts matching, CA principal/name data and stable builder host-key identity are represented distinctly and correctly

### Requirement: Absent configuration preserves legacy bytes

When no builders or controller profiles are declared, existing serialized
aggregate members and projections, deploy outputs and inventory outputs SHALL
remain byte-identical to the baseline. Default-null builder fields SHALL NOT be
added to legacy serialized members; the builders projection namespace SHALL be
absent. The new standalone builder map SHALL be empty. Facet-only declarations
MAY expose authored member capacity but SHALL NOT add a controller projection
namespace. An explicitly declared empty profile SHALL produce empty machine,
SSH and trust data.

The strict existing `deployment.controllerSettings` OS-key schema, precedence,
and projections SHALL remain unchanged. Even with builders configured, this
slice SHALL NOT change deploy target selection, build graph, copy, activation,
or recovery. Native builds SHALL continue using installed daemon defaults.

#### Scenario: Unchanged fleet has unchanged bytes
- **WHEN** the pre-change fake fleet is evaluated without builder configuration
- **THEN** byte comparisons of aggregate, deploy and inventory outputs pass against captured baselines, with no null builder fields or builders namespace added

#### Scenario: Empty profile is explicit
- **WHEN** a valid profile selects no builder lanes
- **THEN** its direct machine list and aggregate machine/SSH/trust collections are empty and existing deploy projections are unchanged

#### Scenario: Deploy identity remains independent
- **WHEN** builder controller routes and deployment controller-platform identities are both configured
- **THEN** builder outputs use only the selected builder profile while deployment retains its existing precedence and OS-key contract

### Requirement: Opt-in platform adapters consume an explicitly selected profile

The system SHALL export nixosModules.builders and darwinModules.builders.
Both SHALL accept mandala.builders.enable, controller, and byController.
The map SHALL be supplied by the consumer from the existing projection, without
re-evaluating fleet members. No OS, hostname or environment inference is allowed.
Disabled modules SHALL contribute no settings. Missing or unknown selection
SHALL fail when enabled.

#### Scenario: Explicit selection
- **WHEN** a consumer enables the adapter with one controller ID and projection map
- **THEN** only that profile supplies daemon settings, SSH and trust

#### Scenario: No fallback
- **WHEN** an enabled adapter selects a missing profile
- **THEN** evaluation fails rather than selecting another controller

### Requirement: Machine ownership is additive unless explicitly exclusive

Enabled adapters SHALL set nix.distributedBuilds and
nix.settings.builders-use-substitutes to true at ordinary priority, and contribute
the selected nix.buildMachines list at ordinary priority. Other module entries
SHALL remain unless exclusiveBuildMachines is explicitly true, which SHALL force
only nix.buildMachines to the selected list.

#### Scenario: Other modules contribute capacity
- **WHEN** another module contributes a machine list
- **THEN** the default adapter preserves it and explicit exclusive mode discards it

### Requirement: Shared pure rendering installs strict system SSH and trust

Both adapters SHALL use one pure renderer for system-wide
programs.ssh.extraConfig and programs.ssh.knownHosts. Rendered aliases SHALL
preserve hostname, login, port, identity, ProxyJump and HostKeyAlias, enforce
IdentitiesOnly yes, IdentityAgent none and StrictHostKeyChecking yes, and close
their Host scope. Pins and CA entries SHALL retain projected names and ports.
The renderer SHALL reject unsafe tokens and non-strict policy before rendering.
It SHALL export exact machines-file text with all standard fields, including
publicHostKey or the standard empty marker, without IFD or secret-file access.
A read-only rendered module option SHALL expose inspection output.

#### Scenario: Shared endpoint lanes and jump routing
- **WHEN** two lanes share a nondefault-port endpoint behind a jump
- **THEN** both machine lines remain and SSH installs trusted target and jump aliases

#### Scenario: Trust encodings are distinct
- **WHEN** pin and CA profiles are rendered
- **THEN** full-key base64 is retained in pin machine lines and CA knownHosts use certAuthority

#### Scenario: Empty and malicious input
- **WHEN** a profile is empty
- **THEN** rendering yields empty text, maps and machine lists
- **WHEN** rendered fields contain injected directives or permissive policy
- **THEN** rendering fails

### Requirement: Tests and claims remain bounded

Golden checks SHALL compare exact text and attrsets for both platforms using
fictional data, preserve slice-1 compatibility baselines, and introduce no
nix-darwin input. Documentation SHALL identify stub-based Darwin evaluation and
defer live route readiness, feature matching and shared-endpoint slot accounting
to qualification on the deployed Nix version.

#### Scenario: Pure adapter checks pass
- **WHEN** synthetic checks pass
- **THEN** they establish rendering parity, not activated daemon or scheduling behavior
