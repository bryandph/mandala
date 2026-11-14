## ADDED Requirements

### Requirement: Publish metadata is an additive safe run projection
Native deploy metadata SHALL add a `publish` object containing `enabled`, `outcome`, `path_count`, optional redacted `cache_url`, optional classified `reason`, and optional numeric `process_rc`. Outcomes SHALL distinguish `pending`, `running`, `succeeded`, `failed`, `skipped` and `interrupted`. Skip reasons SHALL distinguish `disabled`, `no-built-paths` and `build-not-completed`. Updates SHALL use the registry's existing atomic metadata writes. Upload process results SHALL remain nested advisory data, not the run result or a host result. Raw cache URL userinfo/query/fragment, netrc paths and contents, and raw upload stdout/stderr SHALL NOT enter persisted metadata or events.

#### Scenario: enabled run records publication lifecycle
- **WHEN** an enabled native deploy registers, begins publication, and completes its single upload
- **THEN** its metadata progresses from pending to running to the final publish outcome with the unique profile-path count and safe destination

#### Scenario: skipped phase identifies its reason
- **WHEN** publication is disabled, no successful profile exists, or the build aborts before the phase
- **THEN** the metadata records the corresponding skipped reason without launching an upload

#### Scenario: advisory failure does not overwrite the run rc
- **WHEN** Nix upload fails and host deployment proceeds
- **THEN** the numeric upload status, if available, remains `publish.process_rc`, while existing build/controller/host result rules determine the run rc

#### Scenario: old metadata lacks publish fields
- **WHEN** a reader loads run metadata written before publication support
- **THEN** absence of the object means no recorded publish phase and does not change any other judgement

### Requirement: Shared publish events do not create a controller host
The shared publication stream SHALL be `publish.jsonl` using the existing supported event version and event envelope. It SHALL use plugin `publish`, an empty host tag, additive `actor: controller` attribution, and `milestone: publish` records carrying the safe publish outcome data at start and completion. Empty-host shared records SHALL NOT create a host in old or new readers. Eligible successfully built hosts SHALL receive the shared outcome as a non-terminal publish milestone before their existing copy milestone; build-failed hosts SHALL NOT receive false publish success. Unknown additive fields and milestones SHALL remain forward-compatible under the existing version-gating rules.

#### Scenario: controller upload is visible without a synthetic host
- **WHEN** a reader consumes a shared publish milestone
- **THEN** updated readers update the shared publish projection, older readers ignore the empty-host non-build record, and neither adds a controller fleet member

#### Scenario: failed upload milestone is not failed host status
- **WHEN** an eligible host receives a publish milestone whose shared outcome is failed
- **THEN** its host state remains non-terminal and eligible for the existing copy and activation path

#### Scenario: attach reconstructs the shared outcome
- **WHEN** a new reader opens a registry run during or after publication
- **THEN** it can reconstruct the safe shared state from metadata and supported publish events without access to controller credentials

### Requirement: Publish format compatibility is regression gated
In-tree serialization and reader tests SHALL pin the additive metadata and publish milestone shapes, empty-host routing, advisory process-result isolation, legacy absence, unchanged host counts and sticky host outcome precedence. Credential redaction tests SHALL cover persisted state and observer projections. Tests SHALL use isolated temporary state and stub upload programs, not live cache credentials.

#### Scenario: serialization or reader judgement drifts
- **WHEN** an implementation changes the publish shape, invents a controller host, converts advisory failure into a host/run failure, or leaks upload credentials into state
- **THEN** an in-tree serialization or reader-behavior regression gate fails before release
