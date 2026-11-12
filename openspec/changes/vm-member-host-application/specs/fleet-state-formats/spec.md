## ADDED Requirements

### Requirement: Durable VM owner-unit plans and runtime evidence
Deploy metadata SHALL add a VM owner-unit section keyed by owner, containing selected member IDs, requested and affected guest IDs, backend and manifest version, desired/previous/observed immutable owner and guest identities, per-guest readiness/results, staging disposition, primary failure and recovery diagnostics. Before mutation, recovery metadata SHALL be written atomically and referenced previous artifacts SHALL be retained through settlement/reconciliation. Observed state SHALL remain distinct from intended state. Missing evidence SHALL never be reconstructed as verified success.

#### Scenario: attach after controller restart
- **WHEN** an updated frontend attaches to a run after a controller restart
- **THEN** it reconstructs approved owner/guest impact and observed/recovery outcomes from durable records without evaluating the fleet or inferring success from the owner profile

#### Scenario: recovery record cannot be persisted
- **WHEN** the previous runtime record cannot be durably written or its references retained before application
- **THEN** owner mutation does not start and the run records failure

#### Scenario: old run has no VM section
- **WHEN** a reader loads deploy metadata predating typed VM ownership
- **THEN** it preserves prior host judgements and reports no recorded VM evidence rather than inventing guest outcomes

### Requirement: VM event attribution preserves member identity without guest SSH hosts
VM events SHALL carry explicit owner and guest IDs and backend/manifest context in an additive versioned-event envelope. They SHALL remain attributed to the owner application unit, not create independent guest SSH-host tasks or change unrelated host counts. Readers SHALL preserve existing event-version/unknown-field compatibility and tolerate partial trailing records under the shared format contract.

#### Scenario: sibling events share an owner
- **WHEN** two affected guests emit verification and recovery observations
- **THEN** readers show distinct guest outcomes under one owner without counting three independent host transport tasks

#### Scenario: older reader encounters VM evidence
- **WHEN** an older reader sees additive VM fields or an unknown VM milestone
- **THEN** it continues reading supported host records, but its host-only confirmation is not evidence that any guest deployed

### Requirement: VM settlement distinguishes pending and incomplete recovery
Updated registry and frontend projections SHALL determine owner-unit success from owner and affected-guest evidence together. Guest verified/confirmed SHALL require matching observed running generation/image and readiness; explicit staged/pending SHALL be separately counted, never live-confirmed. Rolled-back SHALL require verified previous affected owner state and all previous affected guest runtimes/readiness. Failed, partial-recovery, unknown post-mutation state and recovery-required interruption SHALL remain unsuccessful despite a zero controller exit or confirmed owner profile. Existing failed/rolled-back precedence SHALL be preserved. Writable-data rollback SHALL not be implied.

#### Scenario: runtime recovery is incomplete
- **WHEN** one sibling restores and another has an unknown runtime
- **THEN** both guest observations survive, the owner unit is partial-recovery/failed and the overall result is nonzero

#### Scenario: staging is successful but not deployed
- **WHEN** a boot-only owner update stages guest images without running them
- **THEN** the summary counts pending guests separately and never upgrades them to deployed from the owner's successful staging

### Requirement: VM protocol changes are regression gated
In-tree serialization and reader-behavior tests SHALL pin the additive owner-unit metadata/event shapes, old-record absence, unknown-field/version handling, owner/guest attribution, staged versus verified counts, host-only rollback insufficiency, partial/unknown recovery failure precedence and reconstruction after interruption. Aggregate version-2 support SHALL be explicitly gated before any frontend can deploy a VM-bearing plan.

#### Scenario: a reader loses recovery failure precedence
- **WHEN** a change lets an owner confirmation or zero controller exit mask a failed guest restore
- **THEN** a pinned behavior test fails before release

#### Scenario: an additive writer changes shape
- **WHEN** a deliberate VM serialization change alters canonical bytes or versioned meaning
- **THEN** serialization gates require reviewed expectations and a version bump wherever the existing protocol requires one
