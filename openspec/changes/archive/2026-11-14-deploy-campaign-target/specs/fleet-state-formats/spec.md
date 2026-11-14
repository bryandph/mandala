## ADDED Requirements

### Requirement: Versioned campaign manifests are additive to runs
The system SHALL persist campaigns separately at `campaigns/<campaign-id>/campaign.json` under the state directory. Version 1 SHALL include creation time, immutable content-identified source/selection/host targets/initial exclusions and target digest, plus lifecycle, monotonic generation, exclusion history, attempt links, outcome provenance and verification. Writes SHALL be atomic 1-space sorted JSON without trailing newline. Unknown fields SHALL be tolerated; unsupported schemas, corrupt or missing manifests SHALL refuse mutation and target-dependent success judgments rather than synthesize a target.

#### Scenario: an older reader sees a campaign-linked run
- **WHEN** it reads additive campaign_id, campaign_target_digest, campaign_generation and campaign_attempt metadata
- **THEN** it ignores unknown fields and still reads the existing run outcomes and streams without migration

#### Scenario: future manifest is inspected
- **WHEN** a reader encounters an unsupported campaign schema version
- **THEN** it reports unsupported target data and performs no mutation or clean-target judgment

### Requirement: Attempts preserve run history and recover safely
Each campaign attempt SHALL use a new normal run id and immutable target/exclusion-generation references. Prepared attempt ownership and run linkage SHALL be persisted before host effects. Prior event streams and sticky outcomes SHALL NOT be reset by retry. Shared controller events SHALL use supported event versions, empty host tags and controller attribution. Uncertain interrupted ownership SHALL block duplicate scheduling until reconciliation; campaign state SHALL NOT replace process liveness.

#### Scenario: process disappears during dispatch
- **WHEN** dispatch loses its frontend after publishing an attempt reference
- **THEN** a replacement reconciles the linked supervisor/run and never blindly launches the same work again

#### Scenario: retry succeeds after a failed run
- **WHEN** a failed member succeeds in a later attempt
- **THEN** campaign aggregate state points to the new successful outcome while the original run's failure remains unchanged

### Requirement: Open campaigns protect target evidence from retention
Open campaigns SHALL retain their target roots, outcome evidence and linked runs across ordinary count/age pruning. Closed campaigns and linked inactive runs SHALL be pruned as a unit under existing retention controls using close time; live/supervised allocations SHALL remain protected. Legacy unlinked runs SHALL retain existing retention behavior. Campaign enablement SHALL require retention-aware writers so older pruning cannot invalidate open targets.

#### Scenario: an open campaign is idle between retries
- **WHEN** its linked runs exceed normal retention age or count
- **THEN** its manifest, target roots and linked runs remain available until explicit close

#### Scenario: a closed campaign becomes eligible for pruning
- **WHEN** the closed campaign exceeds retention limits and has no live linked run
- **THEN** its retained group can be removed without leaving an apparently usable dangling target

### Requirement: Campaign formats and judgments are regression gated
Inline golden bytes and shared behavioral fixtures SHALL cover campaign manifests, additive run fields, exclusion revisions, outcome provenance, unsupported schemas, interrupted dispatch and retention, while preserving legacy metadata/event/cache bytes and supported reader judgments.

#### Scenario: a new writer changes canonical campaign bytes
- **WHEN** serialization changes without a reviewed fixture update
- **THEN** the format gate fails before shipping
