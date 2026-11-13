## ADDED Requirements

### Requirement: Immutable campaign target before host effects
An opt-in deployment campaign SHALL bind a frozen, content-identified source graph, exact selected membership, intended per-host profile and system-toplevel paths, captured execution settings, effective activation modes and explicit exclusions before any host effects. Source identity SHALL include revision when available, dirty content identity and resolved nested input/submodule identity; a revision or dirty boolean alone MUST NOT establish equality. Original sourceInfo metadata visible to flake code, including root/input revision, dirty revision, timestamps and revision counts, SHALL be preserved and covered by source graph identity. Capsule implementation bytes SHALL NOT define source equivalence. Capture SHALL always compare the selected members' live and frozen profile/system drvPaths; any mismatch SHALL refuse creation, name the affected members and explain the unreproduced fetcher-metadata dependency. Unsupported Nix capture semantics SHALL refuse rather than degrade. Inconsistent capture or an unresolved intended output SHALL refuse binding and launch no deployment. Real campaign creation SHALL bind the target and start the first deployment attempt in one operation, subject to the same real-mutation confirmation gate as deploy runs and dispatch revalidation of source identity, target digest, exact eligible names and campaign generation. The target and prepared attempt/run link SHALL be durable before host effects. An explicit dry creation preview SHALL bind and display the target without an attempt or host effects, leaving real work pending for retry.

#### Scenario: confirmed creation deploys in one step
- **WHEN** a valid target is captured and real creation is confirmed for its exact eligible members
- **THEN** creation binds that target and starts its first deployment attempt without requiring a separate retry operation

#### Scenario: dry creation preview leaves work pending
- **WHEN** creation is requested as a dry preview
- **THEN** it binds and displays the target without an attempt or host effects and does not mark any member successfully deployed

#### Scenario: fetcher metadata affects a derivation
- **WHEN** a selected member's profile or system output embeds self.dirtyRev, self.lastModified or input fetcher metadata
- **THEN** the capsule preserves the original metadata and capture requires identical live/frozen profile and system drvPaths before binding

#### Scenario: metadata is stripped or the Nix adapter is unsupported
- **WHEN** frozen evaluation cannot reproduce a selected member's live drvPaths or the Nix version is unsupported
- **THEN** capture refuses without a ready campaign or host work and reports the affected members or unsupported version instead of using path-only semantics

#### Scenario: source changes during capture
- **WHEN** source content changes between snapshot capture and target binding
- **THEN** creation reports source-changed, publishes no ready target and starts no host work

#### Scenario: nested source moves without a root commit
- **WHEN** a relevant local input changes or a submodule checkout moves while the root revision remains equal
- **THEN** the source identity differs and cannot be mistaken for the bound target

#### Scenario: live checkout moves after binding
- **WHEN** the checkout changes while a bound attempt is building or deploying
- **THEN** that attempt uses only recorded derivations, outputs and settings from the frozen graph and reports observed movement separately

### Requirement: Campaign retries preserve targets and successes
Every campaign continuation SHALL reuse the immutable target. Deploy retry SHALL schedule only failed, rolled-back or pending eligible members, retaining historical attempts and successful confirmed or reboot-pending outcomes without reactivation. Optional selectors SHALL narrow, never widen membership. Dry activation SHALL NOT satisfy real deployment. Source mismatch or unavailable source identity SHALL explicitly block new deploy/retry/reboot effects and require a new campaign; read-only bound verification SHALL remain available.

#### Scenario: partial failure is retried
- **WHEN** node-a confirmed, node-b failed and node-c remains pending
- **THEN** retry targets only eligible node-b and node-c using their original paths and does not build, copy or activate node-a again

#### Scenario: staged success is not redeployed
- **WHEN** a member previously settled reboot-pending
- **THEN** deploy retry retains its successful staging outcome and leaves reboot to a distinct continuation

#### Scenario: changed source cannot retarget retry
- **WHEN** retry observes source content different from the recorded campaign identity
- **THEN** it reports recorded and observed identities and new-campaign guidance without evaluating a replacement target or launching host work

#### Scenario: exact output is unavailable
- **WHEN** a recorded output and its realizable derivation are unavailable
- **THEN** the attempt reports target-unavailable instead of rebuilding a fresh flake expectation

### Requirement: Exclusions govern every campaign phase
Campaign deploy, retry, reboot and final verification SHALL use the same persistent effective exclusions with exact names and nonempty reasons. Updates SHALL be explicit, generation-checked, attributed and historical; they SHALL NOT change membership, intended paths or prior successes. Updates during a live or uncertain mutating attempt SHALL be refused rather than implying cancellation. Excluded members SHALL remain visible outside eligible outcome denominators.

#### Scenario: an operator skip survives all phases
- **WHEN** operator-alice excludes node-b for maintenance
- **THEN** subsequent deploy, retry, reboot and verification selections omit node-b and display its reason even after frontend restart

#### Scenario: a skip is explicitly removed
- **WHEN** the operator removes node-b's exclusion with a reason and matching generation
- **THEN** only node-b's original eligibility is restored and its target and any prior successful outcome remain unchanged

### Requirement: Campaign reboot and verification are distinct
Campaign reboot SHALL use exact eligible reboot-pending members, fresh evidence that the installed profile equals the bound system target, existing wrapper/order/drain controls and normal confirmation. Final verification SHALL compare fresh observations to bound system paths, including prior successes, without activation. Reboot command success or administrative close SHALL NOT imply final verification. Close SHALL refuse live or uncertain mutation, retain incomplete results and prevent further campaign mutation.

#### Scenario: staged members await proof of boot
- **WHEN** the installed profile equals the target but current differs
- **THEN** staging is successful and reboot-pending but final verification is incomplete

#### Scenario: close records partial completion
- **WHEN** an inactive campaign is closed with failures or exclusions remaining
- **THEN** those outcomes and reasons remain visible and close makes no success claim
