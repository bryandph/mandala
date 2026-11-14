# fleet-state-formats Specification

## Purpose
The on-disk state-directory formats (run registry, deploy metadata, drift snapshots) as a versioned protocol shared by every mandala frontend, with stability gated by in-tree serialization tests.

## Requirements
### Requirement: State-dir formats are a versioned multi-frontend protocol
The on-disk formats under the mandala state directory SHALL be treated as a
compatibility contract shared by every fleet frontend (CLI, TUI, MCP server),
independent of implementation language: the run-registry layout
(`runs/<run-id>/` with `meta.json` and per-plugin `*.jsonl` event streams plus
`output.log` for command runs), the JSONL event protocol (versions 1 and 2,
gated by the `v` field), drift snapshots (`<host>.json` keyed by filename),
the rev-keyed expected cache (`.expected.json`), the per-context discovery
files (under `mcp/`, mode 0600, keyed by canonical checkout, recording
endpoint address, bearer token, leader pid, and flake path — succeeding the
single `mcp/session.json`), and the MCP audit log (`mcp/audit.jsonl`).
Any implementation SHALL ignore event records whose version it does not
support rather than failing, and SHALL treat unknown fields as forward
compatibility, not errors.

#### Scenario: an unsupported event version is skipped
- **WHEN** a frontend tails an event stream containing a record with an
  unrecognized `v` value
- **THEN** the record is skipped and later supported records are still
  consumed

#### Scenario: formats survive an implementation swap
- **WHEN** the porcelain implementation changes (e.g. Python to Rust)
- **THEN** existing registry runs, snapshots, caches, and discovery files
  remain readable without migration, and files it writes remain readable by
  the prior implementation

### Requirement: Format stability gated by in-tree serialization tests
The state-dir formats SHALL remain byte-stable, gated by tests that carry
their expected bytes and judgements in-tree: the canonical serializations
(`meta.json` and `.expected.json` 1-space sorted JSON, the context
discovery files, event JSONL shapes) SHALL be asserted against inline
golden bytes, and reader judgements (drift statuses, liveness, sticky host
states, unknown-event-version skipping) SHALL be pinned by behavior tests.
Registry writes SHALL be atomic (write-then-rename for `meta.json`) and
tailers SHALL tolerate partial trailing lines by re-reading them on the
next poll. A change to any format SHALL fail these gates before it ships;
a deliberate contract change requires a reviewed test-expectation edit and
a version bump where the format is versioned.

#### Scenario: a writer drifts from the golden bytes
- **WHEN** a serialization change alters the bytes written for a canonical
  case
- **THEN** an in-tree golden-byte test fails before the change ships

#### Scenario: a reader drifts from the pinned judgements
- **WHEN** a reader change alters the drift status, liveness, or host-state
  judgement for a pinned case
- **THEN** the behavior test fails, distinguishing a deliberate contract
  change (expectations edited, reviewed) from an accidental regression

### Requirement: Deploy metadata persists per-run boot intent
Every native deploy run SHALL write an additive `boot` boolean to its registry
metadata. Readers SHALL reconstruct absent `boot` fields from older records as
false, and status projections SHALL retain the field wherever run options are
reported.

#### Scenario: new boot run is portable across frontends
- **WHEN** any frontend launches a deploy with the boot override
- **THEN** `meta.json` records `boot: true` and another frontend can attach and
  reconstruct the same run intent

#### Scenario: old record remains readable
- **WHEN** a frontend attaches to deploy metadata written before the `boot`
  field existed
- **THEN** it treats boot override as false without failing or changing the
  record's other judgements

### Requirement: Reboot-pending is an additive successful deploy state
The deploy event protocol SHALL recognize `reboot-pending` as a sticky terminal host state meaning that the expected generation was installed as the next boot default without live activation. Run metadata SHALL add a `reboot_pending` summary count. `confirmed` and `reboot-pending` SHALL contribute to successful settlement, while `failed` and `rolled-back` retain their existing failure precedence. Readers that do not recognize the new milestone or additive summary field SHALL continue processing supported events and fields.

#### Scenario: staged host settles successfully
- **WHEN** a host event stream reaches the reboot-pending milestone and later records status done with rc zero
- **THEN** updated readers retain reboot-pending as the host's successful terminal state

#### Scenario: rollback still determines failure
- **WHEN** one host is reboot-pending and a sibling reaches rolled-back
- **THEN** the first host remains reboot-pending and the overall run is unsuccessful because rollback retains failure precedence

#### Scenario: older reader ignores the new milestone
- **WHEN** a prior reader encounters the additive reboot-pending milestone or summary field
- **THEN** it ignores the unknown data rather than rejecting the remaining run record

### Requirement: State snapshots record the installed system profile additively
Each new fleet state snapshot SHALL include `system_profile`, the resolved installed system-profile target that supplies the next-boot generation. Readers SHALL tolerate snapshots where the field is absent and MUST treat it as observational state rather than proof of live activation.

#### Scenario: new snapshot records a staged generation
- **WHEN** the system profile points at a generation that differs from `/run/current-system`
- **THEN** the snapshot records both paths without claiming that the system-profile generation is running

#### Scenario: legacy snapshot remains readable
- **WHEN** a reader loads a snapshot written before `system_profile` existed
- **THEN** it applies the prior current/booted judgement without a parse or migration failure

### Requirement: Reboot-pending format judgements are regression gated
Canonical serialization and reader-behavior tests SHALL cover the additive `system_profile` snapshot field, reboot-pending milestone and terminal state, `reboot_pending` run-summary count, successful mixed confirmed/reboot-pending settlement, and failure precedence when failed or rolled-back hosts are present.

#### Scenario: a reboot-pending judgement drifts
- **WHEN** an implementation stops treating reboot-pending as successful and terminal or loses the staged system-profile judgement
- **THEN** an in-tree serialization or behavior gate fails before release

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

### Requirement: Copy telemetry is additive per-host state
Host streams SHALL support additive `copy` events with `substitute_on_destination`, optional `duration_ms`, optional `success`, optional `paths_copied` and optional `controller_nar_bytes`. Start events SHALL omit completion fields. Counts SHALL be unsigned Nix-reported completed paths and cumulative uncompressed NAR payload bytes, not closure estimates or wire bytes. Registry, CLI status, TUI and MCP status SHALL expose the latest optional per-host copy projection. Existing event versions and sticky host outcomes SHALL remain unchanged.

#### Scenario: reattach after completion
- **WHEN** a reader opens a completed run
- **THEN** host events reconstruct mode, duration and available counts without another Nix invocation

#### Scenario: legacy host stream
- **WHEN** older records contain only copy milestones
- **THEN** copy measurements are absent and existing host-state judgement is unchanged

#### Scenario: explicit empty progress
- **WHEN** Nix explicitly reports zero completed paths or bytes
- **THEN** those reported zero values are preserved

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

### Requirement: Durable VM owner-unit plans and runtime evidence
Deploy metadata SHALL add a VM owner-unit section keyed by owner, containing selected member IDs, requested and affected guest IDs, backend and manifest version, desired/previous/observed immutable owner and guest identities, per-guest readiness/results, staging disposition, primary failure and recovery diagnostics. Before mutation, recovery metadata SHALL be written atomically and referenced previous artifacts SHALL be retained through settlement/reconciliation. Observed state SHALL remain distinct from intended state. Missing evidence SHALL never be reconstructed as verified success.

VM-specific metadata SHALL retain the approved plan, inventory/frozen-source identity, exact fingerprint and settings/mode/target context. Readers SHALL recompute that binding before granting verified status; missing or changed approval evidence SHALL be unknown/unsuccessful, not an owner-only success.

#### Scenario: approved context is missing or changed
- **WHEN** a run retains a confirmed owner journal but its approval, revision or effect context is missing or inconsistent
- **THEN** attachment reports unknown/unsuccessful VM evidence without evaluation or host effects

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
