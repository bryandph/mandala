## ADDED Requirements

### Requirement: Darwin owner records distinguish artifact and runtime identity
The parent VM owner-unit records SHALL add the Darwin adapter tag, manifest/capability version, explicit profile path, installed wrapper/underlying system mapping, desired/staged/observed guest images, runtime lineage/boot epoch/disk incarnation, readiness, retained recovery references and data/key limits. Desired state SHALL remain distinct from owner-observed runtime evidence. Atomic pre-mutation records SHALL survive controller loss and SHALL support reconstruction without fleet reevaluation. Private guest keys SHALL NOT enter deploy records.

#### Scenario: observer attaches after controller loss
- **WHEN** another frontend attaches to an interrupted Darwin owner run
- **THEN** it reconstructs the approved sibling impact and persisted evidence/unknown recovery status without assuming profile success or reevaluating configurations

#### Scenario: state cannot be persisted
- **WHEN** prior runtime retention or atomic recovery metadata writing fails
- **THEN** the owner transaction refuses mutation and records failure

#### Scenario: regenerated key
- **WHEN** re-image creates a new guest host key
- **THEN** metadata records the runtime/disk incarnation and declared key-regeneration limitation without storing private key material or a guest SSH target

### Requirement: Darwin settlement does not reuse NixOS boot proof
Updated readers SHALL distinguish Darwin live owner confirmation, explicitly pending guests, verified runtime restoration and failed/partial/unknown recovery under the existing VM precedence rules. Wrapper installation alone SHALL NOT prove current-system activation or guest readiness and SHALL NOT be represented as a NixOS next-boot default. Unsupported Darwin boot intent SHALL be rejected rather than converted into successful reboot-pending. Host-only confirmation and a zero controller exit SHALL NOT mask missing guest evidence.

#### Scenario: pending guest with active owner
- **WHEN** the owner is verified live but an explicitly staged guest still runs its previous image
- **THEN** readers count the guest pending separately and do not claim that all guests deployed

#### Scenario: profile-only recovery
- **WHEN** the owner profile was restored but a guest disk lineage is lost
- **THEN** readers retain unsuccessful recovery regardless of a host rollback milestone

#### Scenario: missing observation
- **WHEN** runtime observations are absent after a post-mutation disconnect
- **THEN** readers show unknown/recovery-required state rather than reconstructing success from intent

### Requirement: Darwin protocol additions have pinned compatibility judgments
Serialization and reader tests SHALL pin new Darwin fields, wrapper/system mapping, pending/deployed separation, old-record absence, unknown-field/event handling and incomplete recovery failure precedence. Existing non-opted-in records SHALL preserve their meanings and bytes where unchanged. Aggregate version-2 deployment readers SHALL explicitly recognize adapter capability before effects; permissive event-field compatibility SHALL NOT authorize an unknown deployment adapter.

#### Scenario: old run without Darwin fields
- **WHEN** a reader loads a pre-Darwin record
- **THEN** it applies existing host/VM judgments without inventing Darwin runtime evidence

#### Scenario: additive writer changes
- **WHEN** canonical Darwin metadata bytes or versioned semantics change
- **THEN** in-tree gates require reviewed expectations and any protocol-required version bump

#### Scenario: old event reader
- **WHEN** an older frontend encounters additive Darwin fields or unknown milestones
- **THEN** it continues processing supported records but its host-only outcome is not guest deployment evidence
