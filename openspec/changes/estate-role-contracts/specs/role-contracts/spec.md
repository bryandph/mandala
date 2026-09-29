## ADDED Requirements

### Requirement: Experimental role scope

mandala SHALL export `roles.org` and `roles.project` as experimental flake-parts modules. The other six estate roles SHALL remain documented future architecture and SHALL NOT be published as conforming interfaces in this change.

#### Scenario: Role catalog
- **WHEN** a consumer inspects the role exports
- **THEN** it SHALL find org and project modules, with their experimental status documented

### Requirement: Discoverable role metadata

Each module SHALL publish JSON data at `mandala.role` containing `name`, integer `contractVersion`, `provider = { name, source }`, and `engineSource`. Source fields SHALL be derived source-tree narHash strings. Conflicting role declarations SHALL fail. Metadata inspection SHALL NOT build a derivation or force the complete configuration graph.

#### Scenario: Metadata only
- **WHEN** an agent evaluates `mandala.role` as JSON
- **THEN** it SHALL receive metadata without building, with no implication that every conformance assertion has run

### Requirement: Exact required outputs

The org role SHALL publish fully validated data at `mandala.contract.org`. The experimental project role SHALL publish `mandala.contract.project = { name, orgName, site }`, bind exactly one org. Name and orgName SHALL be non-empty strings; site SHALL be null or a valid selected site. A separate integration fixture SHALL expose `nixosConfigurations.contract` using the adapter; ordinary project implementers SHALL NOT be required to expose NixOS configurations.

#### Scenario: Missing required data
- **WHEN** a project omits its required project name
- **THEN** its conformance check SHALL fail naming the role and missing field

#### Scenario: Multiple org bindings
- **WHEN** a project binds two organizations
- **THEN** validation SHALL fail naming the unsupported composition

### Requirement: Input API compatibility

The project role SHALL require its org binding to publish role org and supported contract version 1. Validation SHALL fail with the binding name, expected interface, and actual interface when either differs.

#### Scenario: Wrong input role
- **WHEN** the org binding publishes role project
- **THEN** conformance SHALL fail with a named role mismatch

#### Scenario: Unsupported version
- **WHEN** the org binding publishes an unsupported contract version
- **THEN** conformance SHALL fail naming the version and supported set

### Requirement: Proxy provenance and source agreement

A fixture proxy SHALL re-export the project module through a wrapper supplying provider metadata. The project engine source SHALL equal the org engine source independently of API-version validation. No check SHALL claim that equal versions prove a follows relationship.

#### Scenario: Agreed sources
- **WHEN** the proxy engine follows the org engine
- **THEN** the project SHALL conform and record the proxy as provider

#### Scenario: Same version with different sources
- **WHEN** org and proxy use different engine source hashes advertising the same supported contract version
- **THEN** conformance SHALL fail naming the source mismatch and suggesting the canonical follows path

#### Scenario: Missing source metadata
- **WHEN** a binding lacks required source metadata
- **THEN** validation SHALL fail with a named metadata error rather than silently accepting the binding

### Requirement: Bounded conformance and fleet compatibility

Each role SHALL add conformance checks that force its contract data and relevant adapter assertions without recursively evaluating the complete flake. Existing fleet aggregate values and shapes SHALL remain unchanged.

#### Scenario: Existing consumer
- **WHEN** the unchanged showcase evaluates against the candidate engine
- **THEN** its aggregate SHALL match the recorded baseline

### Requirement: Fictional candidate fixtures

Direct and proxy project fixtures SHALL consume fictional orgs and the candidate engine. Root inputs SHALL be exactly nixpkgs and the structural flake-parts/import-tree inputs, as the modified `mandala-engine` purity invariant requires. This slice SHALL require no delivery-tool input.

#### Scenario: Candidate graph
- **WHEN** CI overrides the fixture engine to the candidate implementation
- **THEN** both the org and proxy engine paths SHALL resolve to that candidate
