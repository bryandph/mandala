## MODIFIED Requirements

### Requirement: Host-application backend dependencies are consumer-injected
The engine SHALL define `nixos-module` and `darwin-module` contracts without pinning virtualization or nix-darwin toolchains or generating guest systems. Consumers SHALL supply supported owner modules, configuration graphs and immutable owner artifacts referencing guest outputs. Pure projection validation SHALL reject ownership/manifest mismatches and unsupported owner/backend/system combinations before effects. The Darwin application adapter SHALL be explicitly tagged `darwin-system`; unsupported adapters SHALL be rejected before build or mutation rather than lowered as NixOS.

#### Scenario: Darwin toolchain is injected
- **WHEN** a consumer supplies its Darwin owner configuration, Linux guest configuration and qualified runtime module
- **THEN** Mandala projects references in their single graph without adding toolchain inputs or another configuration authority

#### Scenario: artifact names an unrelated guest
- **WHEN** an owner manifest references a guest owned by another member
- **THEN** pure validation rejects it before runtime effects

#### Scenario: old reader does not support Darwin adapter
- **WHEN** a version-2-aware deployment reader encounters an unsupported darwin-system adapter
- **THEN** it refuses before build or transport instead of assuming a NixOS profile

## ADDED Requirements

### Requirement: Darwin owner projections preserve one graph and transport guards
Deploy-node and batch projections SHALL accept explicitly enrolled Darwin owner configurations additively, emitting one activatable system-profile unit per eligible owner. Native selection SHALL use the parent VM owner expansion, impact/exclusion and approval rules. Raw static batches SHALL NOT implicitly expand guest-only groups. All entry points and controller-platform variants SHALL exclude VMs from Ansible SSH, deploy-rs nodes, independent batches and builder routes, including after hooks. Owner artifacts SHALL reference immutable guest dependencies in the same graph and single batch build. The aggregate SHALL contain only pure adapter/capability metadata and installable strings, never realized derivations.

#### Scenario: mixed-system build
- **WHEN** a Linux guest selection resolves to a Darwin owner alongside a selected NixOS host
- **THEN** one batch invocation builds their owner/host artifacts and transitive dependencies and only complete owner artifacts may be copied

#### Scenario: direct raw group has only a guest
- **WHEN** a static batch projection sees a group containing only a VM
- **THEN** it emits neither a guest entry nor silently substituted owner entry

#### Scenario: hook tries to restore guest transport
- **WHEN** a consumer hook or direct projection input attempts to emit the VM as a transport or builder target
- **THEN** every relevant projection still excludes that route

### Requirement: Darwin controller identities and local capacity retain existing semantics
Every owner effect SHALL use the existing flattened connection/privilege settings selected by runtime controller OS, preserving fleet < controller < sorted groups < member precedence, absolute identity-path strings and strict authored-key rules. The target OS SHALL NOT choose the controller identity. Missing configured OS entries SHALL fail without ambient fallback. Self-deployment SHALL use the authenticated owner SSH endpoint without implicit local bypass and SHALL refuse if activation can terminate the controlling process or destroy required recovery state. Guest-backed capacity SHALL be advertised only on the owner's `host.builder`; controller member-ID self-exclusion SHALL leave local daemon capacity available without a guest route.

#### Scenario: Mac deploys itself
- **WHEN** a Darwin controller explicitly selects its own Darwin owner unit with a safe independent controller process
- **THEN** owner effects use the darwin controller identity and normal owner SSH/privilege path, and builder self-exclusion leaves its Linux capacity local

#### Scenario: Linux controller deploys the same Mac
- **WHEN** a Linux controller deploys the Darwin owner
- **THEN** every owner operation uses the linux controller identity subject to group/member overrides and strict owner trust

#### Scenario: configured identity is missing
- **WHEN** the runtime controller OS has no entry in a configured controllerSettings map
- **THEN** deployment refuses before effects and does not select identity from the target's Darwin platform

#### Scenario: unsafe self-deployment
- **WHEN** the owner module would stop the controller or remove its recovery artifacts during self-deployment
- **THEN** preflight refuses mutation rather than relying on an unowned background recovery process

#### Scenario: guest capacity dispatch
- **WHEN** another controller schedules a supported Linux lane advertised by a Darwin owner
- **THEN** it connects only to the owner's strict builder endpoint and the owner's daemon dispatches the work without a fleet guest route or duplicate advertisement
