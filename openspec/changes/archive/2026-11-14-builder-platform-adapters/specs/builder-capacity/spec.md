## ADDED Requirements

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
