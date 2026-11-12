## MODIFIED Requirements

### Requirement: Single module-managed backend applies prebuilt guests through their owner
The system SHALL support the `nixos-module` backend for qualified NixOS owner/NixOS guest pairs and the additive `darwin-module` backend for qualified Darwin owner/Linux NixOS guest pairs. Each consumer-injected module SHALL expose a versioned typed manifest and bounded inspect, prepare, apply, verify and restore operations identifying owner, guest, runtime instance, desired/previous/observed immutable generation/image, supported systems, readiness and dispositions. All transfer/runtime control SHALL use only the owner's flattened transport/privilege settings, never guest SSH or guest-exec. Neither backend SHALL evaluate or build during activation.

#### Scenario: NixOS module remains supported
- **WHEN** a qualified NixOS module restarts its prebuilt guest
- **THEN** it retains existing owner-only application semantics without a Darwin dependency

#### Scenario: Darwin module regenerates a guest
- **WHEN** an approved Darwin owner configuration change selects a different immutable Linux image
- **THEN** the owner module may instantiate a replacement runtime/disk from that prebuilt image within its declared recovery contract, without evaluating or rebuilding the guest

#### Scenario: only desired state is observable
- **WHEN** a backend reports only its configured image pointer or owner profile
- **THEN** it fails runtime-evidence conformance and cannot claim guest deployment

### Requirement: Guest deployment requires running identity and readiness
A guest SHALL be deployed/confirmed only after the requested generation/image is observed running and bounded declared readiness checks pass. Explicit stage-only images SHALL remain pending, separately counted and never deployed. Failed live restart/re-image or readiness expiry SHALL fail rather than become pending. NixOS target-owned preflight and transactional boot-only guarantees SHALL remain authoritative for NixOS owners. Darwin SHALL use its declared adapter dispositions and SHALL NOT invoke NixOS checks/boot actions. Supported dry activation SHALL allow build/copy and non-mutating inspection only, with preview/not-applied outcomes and no profile, runtime, disk or recovery-state mutation.

#### Scenario: identity matches but readiness fails
- **WHEN** a running guest matches the desired image but fails readiness
- **THEN** the guest is not confirmed and the owner's recovery policy applies

#### Scenario: NixOS inhibitor refuses
- **WHEN** a NixOS owner pre-switch check refuses live activation
- **THEN** only transactional boot staging is allowed and guests are not restarted to bypass the inhibitor

#### Scenario: Darwin explicit guest staging
- **WHEN** an approved Darwin plan explicitly stages a guest image without replacing the running instance
- **THEN** the guest is pending and separately counted while the owner must still satisfy its own live-activation checks

#### Scenario: Darwin dry preview
- **WHEN** a Darwin owner is dry-activated through a supported preview path
- **THEN** no profile, launchd job, guest disk/runtime or runtime recovery state changes and no guest is reported deployed

### Requirement: Deploy settings fidelity
The engine SHALL honor flattened endpoint/login/port/identity/options, target profile user, sudo, rollback flags, substitution policy, timeouts and temporary-path settings for the applicable adapter; Darwin system-profile activation SHALL require root. Authored identity SHALL govern every owner copy, inspection, lock, activation, verification, confirmation and recovery operation with `IdentitiesOnly=yes` and `IdentityAgent=none` as specified by the existing contract. NixOS switch/boot behavior SHALL remain unchanged. Darwin SHALL require switch mode and an explicit supported dry path; unsupported modes SHALL fail before copy/mutation rather than execute a NixOS command or silently ignore the setting.

#### Scenario: Darwin timeout overrides
- **WHEN** a Darwin owner overrides activationTimeout and confirmTimeout
- **THEN** bounded module verification and the deploy-rs controller protocol use the resolved limits with attributed diagnostics

#### Scenario: explicit owner privilege
- **WHEN** a non-root SSH login deploys the Darwin root system profile
- **THEN** the declared sudo path performs owner operations as root without changing controller-key selection

#### Scenario: unsupported target profile user
- **WHEN** a Darwin owner requests a non-root system-profile target user
- **THEN** adapter validation refuses before mutation instead of activating a user profile as the system

### Requirement: Per-run boot activation override
The native deploy engine SHALL preserve the per-run boot override and existing per-member NixOS semantics. Because the first Darwin adapter has no boot-only owner operation, an explicit boot override or Darwin member boot mode SHALL reject the resolved plan before copy/mutation, naming the unsupported unit. It SHALL NOT stage a Darwin system through switch-to-configuration or silently live-switch it. A revised NixOS-only selection SHALL retain normal boot behavior.

#### Scenario: mixed plan requests boot
- **WHEN** a selection containing a Darwin owner requests boot mode
- **THEN** plan validation refuses before effects and asks for a supported revised selection rather than partially starting the plan

#### Scenario: NixOS-only boot is unchanged
- **WHEN** the revised plan contains only NixOS units and requests boot
- **THEN** existing transactional boot staging and reboot-pending outcomes apply

## ADDED Requirements

### Requirement: Darwin owner artifact has bounded deploy-rs activation
The Darwin owner artifact SHALL wrap the prebuilt nix-darwin system with Darwin-native activate-rs and deploy-rs-activate entry points plus its typed guest manifest. The adapter SHALL explicitly select the root system profile and distinguish installed wrapper path from underlying active system path. Custom activation SHALL execute prebuilt root Darwin activation and required guest application/verification before successful return permits canary creation. The controller SHALL preserve deploy-rs canary/wait/confirm, stale-sentinel handling, owned child processes and bounded rollback waiting. Unknown protocol capability or an incompatible previous wrapper SHALL refuse mutation.

#### Scenario: canary follows guest verification
- **WHEN** Darwin activation and all required guest checks succeed
- **THEN** activate-rs may expose the canary and the controller confirms through the existing protocol, not a separate guest confirmation channel

#### Scenario: guest check fails
- **WHEN** owner script activation succeeds but guest verification fails
- **THEN** the wrapper fails before successful confirmation and complete owner-unit recovery is required

#### Scenario: wrapper differs from current-system
- **WHEN** the installed system profile points at a deploy wrapper and current-system points at its underlying nix-darwin system
- **THEN** verification uses the recorded mapping rather than treating unequal paths as drift or treating the wrapper alone as runtime proof

#### Scenario: first adoption has no compatible rollback entry point
- **WHEN** the previous Darwin profile lacks a compatible restore wrapper or retained guest runtime
- **THEN** the adapter refuses before mutation and requires separately approved bootstrap rather than promising deploy-rs recovery

### Requirement: Darwin guest evidence is owner-observed and incarnation-aware
The darwin-module adapter SHALL observe the active guest generation/image, runtime lineage, boot epoch and disk incarnation from authoritative owner-local VM-manager state or a root-owned observation manifest joined to that active instance. Readiness SHALL include declared bounded runtime/service checks beyond power-on. Stale, desired-only, stopped or ambiguous observations SHALL fail verification. Re-image key changes SHALL NOT introduce guest transport, builder trust, TOFU or pin requirements.

#### Scenario: stale manifest
- **WHEN** a runtime manifest describes the desired image but belongs to a stopped or superseded VM instance
- **THEN** the guest is not confirmed even if the owner profile matches

#### Scenario: wrong image has a running process
- **WHEN** the VM manager reports a healthy process for an image different from the approved artifact
- **THEN** deployment verification fails

#### Scenario: replacement regenerates disk and key
- **WHEN** approved re-imaging starts a new disk/key incarnation with the desired image and passes readiness
- **THEN** the guest may be deployed with recorded lineage while its key is not used as a fleet SSH identity

### Requirement: Darwin recovery restores retained prior runtime lineage
Before mutation the adapter SHALL atomically persist and retain prior wrapper/system/manifest paths and every affected guest's runtime lineage, image, disk incarnation and readiness contract under the owner lock. The module SHALL retain a restartable prior disk/runtime or a qualified equivalent snapshot; destructive-only replacement, absent prior runtime or unavailable retention SHALL refuse mutation. With automatic rollback enabled, failure SHALL restore and verify affected prior owner state and all prior guest lineages/readiness before rolled-back. A new boot epoch MAY differ, but an old image on a fresh unrelated disk SHALL NOT count as prior-runtime restoration. Profile-only magic rollback SHALL NOT establish owner-unit recovery. Auto-rollback disabled SHALL remain failed/unrecovered.

Writable application data, arbitrary activation effects, external state, in-flight jobs and host-key restoration SHALL NOT be promised by image/profile reversal. Limits SHALL be previewed before approval. Partial restoration or unknown state SHALL remain unsuccessful with durable primary/recovery diagnostics and reconciliation before another owner mutation. Independent successful owners SHALL not be revoked.

#### Scenario: old image on a fresh disk
- **WHEN** profile rollback recreates a guest from the old image but has lost the previous disk/runtime incarnation
- **THEN** recovery is incomplete and cannot be labelled rolled-back

#### Scenario: retained runtime is restored
- **WHEN** the prior wrapper and retained guest disk/lineage are restored and prior-image/readiness checks pass for every affected guest
- **THEN** the unit is rolled-back with explicit data/key limitations and the failed deployment remains unsuccessful

#### Scenario: destructive module cannot recover
- **WHEN** prepare detects that re-imaging overwrites the sole prior disk or irretrievably loses required runtime identity
- **THEN** mutation is refused before profile or guest changes

#### Scenario: confirmation loss and partial recovery
- **WHEN** confirmation fails and one affected guest cannot be observed after restore
- **THEN** host magic rollback alone is insufficient, the unit is partial-recovery or unknown and the run is nonzero

#### Scenario: no automatic recovery requested
- **WHEN** activation fails with autoRollback disabled
- **THEN** records preserve failed/unrecovered state rather than reporting rolled-back

### Requirement: Darwin owner conformance is effect-isolated
Mandatory tests SHALL use fictional fake fleets and injected build, transport, runtime and persistence effects. They SHALL pin enrollment/collision checks, controller identity, one graph/batch, owner impact approval, cross-controller owner serialization, wrapper/system mapping, dry/boot behavior, incarnation-aware evidence and retained-runtime recovery. They SHALL assert no guest transport, activation-time construction or live fleet mutation. Darwin watcher/module/daemon live qualification SHALL remain a separate explicit gate, not inferred from mocks.

#### Scenario: prohibited guest effect
- **WHEN** a fake deployment attempts guest SSH, guest-exec, a second build or activation-time evaluation
- **THEN** conformance fails on the recorded effect trace

#### Scenario: two controllers contend
- **WHEN** barrier-controlled fake runs select sibling guests through different controllers
- **THEN** one owner transaction holds the shared lock through recovery and the other reports bounded wait/busy without racing

#### Scenario: isolated tests pass
- **WHEN** pure and injected-effect checks pass on a Darwin workstation
- **THEN** they do not claim live VM-manager, filesystem-watcher rollback or daemon-dispatch qualification
