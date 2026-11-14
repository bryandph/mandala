# fleet-deploy Specification

## Purpose
Native fleet deployment: a selection evaluates and builds once, then activation fans out in parallel per host from the prebuilt profiles with per-host rollback, deploy-settings fidelity, target-owned live-switch preflight, and transactional boot-only staging. Static `deployBatch.<group>` outputs remain the cache-warming surface.

## Requirements
### Requirement: Eval-once batch deploy
The system SHALL evaluate a deploy selection once: settings resolve from the
versioned aggregate, exactly the resolved standalone/owner application-unit installables build
in one `nix build` invocation, and activation fans out from those prebuilt
profiles with NO further flake evaluation — no per-host `deploy` CLI re-evals
and no unrelated-member builds. A selected VM SHALL resolve to its owner artifact, whose transitive guest/sibling closure or image dependencies SHALL belong to the same consumer fleet graph and batch build. The pure aggregate SHALL carry references, not derivations; built owner manifests SHALL reference immutable guest outputs. Guest activation SHALL NOT evaluate or rebuild a guest configuration. Static `deployBatch.<group>` linkFarms remain
the cache-warming surface rather than the runtime selection API.

#### Scenario: a group deploy evaluates once
- **WHEN** a multi-host selection is deployed
- **THEN** the flake is evaluated a single time, profiles build once, and
  each standalone/owner unit activates from its prebuilt artifact without triggering another
  evaluation

#### Scenario: cache warming is unchanged
- **WHEN** `deployBatch.<group>` is built and pushed by the cache-warm app
- **THEN** the artifact shape and push flow are identical to the pre-engine
  behavior

#### Scenario: guest dependencies build with their owner
- **WHEN** a guest-only selection resolves to an owner whose artifact references guest and sibling images
- **THEN** one batch invocation builds that owner artifact and its dependencies, and owner application performs no guest evaluation or image build

#### Scenario: a guest dependency fails construction
- **WHEN** a guest closure dependency fails while an independent owner's artifact builds
- **THEN** the incomplete owner unit is never copied or applied, and existing best-effort policy may proceed with the independent verified artifact

### Requirement: Best-effort profile builds
The native deploy engine SHALL resolve the selected profile derivations once
before building. By default it SHALL attempt independent derivations after a
build failure, verify the exact successful outputs without rebuilding or
re-evaluating the flake, and deploy only those successful profiles. Failed
targets SHALL have sticky failed host outcomes; the overall run SHALL remain
nonzero even when healthy targets deploy successfully. Failed derivations and
log tails SHALL be reported, with available full logs retained in the run
registry and unavailable logs identified explicitly.

#### Scenario: some profiles fail to build
- **WHEN** one selected profile fails and another builds successfully
- **THEN** the successful profile is deployed, the failed profile is never
  copied or activated, and the summary identifies the failed target

#### Scenario: every profile fails to build
- **WHEN** no selected profile is successfully built
- **THEN** no deployment starts and every selected target is reported failed

#### Scenario: operator opts into halting
- **WHEN** the operator enables Halt on build failure in the Shift+D dialog
  or passes `--halt-on-build-failure` to a deploy command
- **THEN** the build stops on failure and no target is deployed
- **AND** the option is disabled by default in each new deploy dialog

#### Scenario: cancellation or resolution failure
- **WHEN** profile resolution fails or the build is interrupted
- **THEN** no deployment starts

### Requirement: Per-host rollback preserved
The system SHALL keep deploy-rs as the per-host activation primitive: the
engine's vendored deploy-rs push/activate pipeline speaks the unmodified
magic-rollback protocol (canary lock, concurrent wait, confirm-by-rm) to
the `activate-rs` closure the projection builds into every profile. Rollback is PER HOST: a failed host rolls back alone and sibling
hosts proceed — the engine SHALL NOT revoke previously-succeeded hosts on a
later host's failure.

For owners with VMs, deploy-rs SHALL remain the host profile primitive but SHALL NOT alone establish rollback success. The backend SHALL additionally restore and verify all affected previous guest runtimes and readiness, plus affected owner state. Failure or incomplete verification SHALL remain failed/partial-recovery rather than rolled-back. Independent owner units retain the existing isolation policy.

#### Scenario: a bricked host rolls back
- **WHEN** activation on one host fails its confirm within the timeout
- **THEN** activate-rs restores that host's previous generation, and the run
  reports the rollback as that standalone host's sticky terminal state; a VM owner additionally requires verified guest restoration

#### Scenario: activation cancels its waiter before rollback completes
- **WHEN** activate-rs signals cancellation and the concurrent waiter fails
- **THEN** the controller keeps activation and its output pipes alive until it
  finishes, bounded by `activationTimeout` (240 seconds when unset)
- **AND** a nonzero activation exit with `autoRollback` enabled is a typed
  host-profile rollback outcome; a VM owner requires additional verified guest runtime restoration before the unit is rolled-back; with auto-rollback disabled, or if the bound expires, it is failure
- **AND** termination or expiry still kills the owned local activation group

#### Scenario: retrying a closure with stale sentinels
- **WHEN** a magic-rollback attempt starts for a previously attempted closure
- **THEN** both its deterministic canary and cancel files are removed before
  activation or the waiter starts, including with older activate-rs versions

#### Scenario: a failure does not revoke siblings
- **WHEN** one host of a multi-host run fails and rolls back
- **THEN** hosts that already confirmed stay on the new generation, pending
  hosts continue, and the run summary reports the partial outcome loudly

### Requirement: Fleet deploy result includes host outcomes
The fleet deploy pipeline SHALL report an overall failure when the controller process exits non-zero or any targeted host reaches the sticky `failed` or `rolled-back` state. `confirmed` and `reboot-pending` SHALL both be successful terminal states, but summaries MUST count them separately and MUST NOT describe a reboot-pending host as live-confirmed. A zero controller exit MUST NOT convert a failed or rolled-back host outcome into overall success.

For VM owner units, host confirmation SHALL NOT imply guest confirmation. The result SHALL include each affected guest's verified, pending, failed and recovery disposition. A failed guest, partial recovery, unknown post-mutation runtime or recovery-required interruption SHALL make the run unsuccessful. Explicitly staged guests MAY settle as successful pending work, separately counted and never described as deployed. Only a verified previous runtime and readiness may count as rolled-back.

#### Scenario: mixed fan-out rolls one host back
- **WHEN** the controller process exits zero after one target confirms and another target rolls back
- **THEN** the overall deploy result is non-zero/failed while both per-host outcomes remain visible

#### Scenario: every host confirms
- **WHEN** the controller process exits zero, every reported owner/standalone target confirms and every affected guest is runtime-verified and ready
- **THEN** the overall deploy result is successful and may report that all targets confirmed

#### Scenario: successful run includes staged hosts
- **WHEN** a run finishes with confirmed and reboot-pending hosts, no failed or rolled-back host, and no failed, partial-recovery or unknown affected guest
- **THEN** the overall deploy result is successful while the summary reports both counts and does not claim all hosts confirmed

#### Scenario: host confirms but guest readiness fails
- **WHEN** host activation confirms but an affected guest does not become ready within its declared bound
- **THEN** the owner unit and run fail or report verified rollback, never deployed based on host confirmation

#### Scenario: image is staged but not running
- **WHEN** boot-only staging installs a desired image without restarting its guest
- **THEN** the guest is pending, counted separately from verified deployment even when staging succeeds

### Requirement: Native parallel fan-out
The deploy engine SHALL orchestrate fan-out in-process: a throttled pool of
per-host deploy tasks, each writing its own attributed event stream. A
`--limit` SHALL remain required and resolve through the canonical taxonomy
spelling; selected standalone members without `deployRs.enable` are skipped with an
explicit notice, while VMs resolve to directly managed owner units despite disabled guest SSH surfaces; a selection with zero deployable members is refused before
a run is created. An abnormal failure (including a panic) in one owner/standalone task SHALL fail only that unit. The full copy/apply/verify/recover transaction SHALL serialize per owner across runs, controllers and direct-owner/guest selections using an owner-enforced lock, while independent owners retain bounded parallelism. Pending crash recovery SHALL be reconciled before a new owner mutation.

#### Scenario: throttle bounds concurrency
- **WHEN** a selection larger than the throttle deploys
- **THEN** at most `--throttle` hosts are in flight at once and every host
  is eventually processed

#### Scenario: a non-deploy member is skipped loudly
- **WHEN** the selection includes a standalone member with ansible management but
  `deployRs.enable = false`
- **THEN** the run proceeds for the deployable members and reports the
  skipped member by name

#### Scenario: a task crash is contained
- **WHEN** one host's deploy task fails abnormally mid-run
- **THEN** that host is reported failed and every other host's deploy
  completes normally

#### Scenario: concurrent sibling deployments contend for one owner
- **WHEN** two runs select different guests of the same owner
- **THEN** one owner transaction holds the shared lock and the other waits within a bound or reports busy, never concurrently changes the owner module

### Requirement: Deploy settings fidelity
The engine SHALL honor the contract's flattened per-node deploy settings at execution: SSH endpoint/login user/port/identity-file/opts, target profile `user`, `sudo`, `magicRollback`, `autoRollback`, `fastConnection`, `confirmTimeout`, `activationTimeout`, `tempPath`, and per-member activation mode (`switch`/`boot`). Switch-mode members SHALL use target-owned preflight when supported; boot-mode members SHALL use transactional boot-only staging and finish reboot-pending. When an identity is authored, copy, preflight, staging, activation, and confirmation commands SHALL receive it explicitly; execution SHALL not depend on the invoking process's `HOME`, effective local user, ambient SSH agent, or default SSH key search for that host. An authored identity SHALL disable `IdentityAgent` for native and raw deploy-rs connections so the key file itself is authoritative.

#### Scenario: a member timeout override takes effect
- **WHEN** a member authors `confirmTimeout` above the fleet default and its generation passes preflight
- **THEN** that host's live activation waits the member's value before rolling back while other hosts use their own resolved values

#### Scenario: activation mode is honored per member
- **WHEN** a selection mixes members with `activation = "switch"` and `activation = "boot"`
- **THEN** safe switch-mode members activate and confirm live while boot-mode members stage without activation and finish reboot-pending in the same run

#### Scenario: explicit identity survives a root supervisor
- **WHEN** the engine process has a different effective user or home from the operator who owns the configured identity file
- **THEN** copy and every remote disposition command use the configured login user, endpoint, port, and identity-file path without requesting its signature from an ambient agent

### Requirement: Per-run boot activation override
The native deploy engine SHALL accept a per-run boot activation override. When
the override is false or absent, each host SHALL retain its flattened
per-member activation mode. When the override is true, every selected
deployable host SHALL use the engine's boot activation path without changing
the authored fleet contract.

#### Scenario: run override selects boot for switch members
- **WHEN** a deploy run requests boot mode for members whose flattened
  activation is `switch`
- **THEN** each selected deployable member is activated through the existing
  boot path and no live switch is requested

#### Scenario: default preserves mixed member modes
- **WHEN** a deploy run does not request the boot override and its selection
  mixes `switch` and `boot` members
- **THEN** each member uses its own flattened activation mode exactly as before

#### Scenario: dry boot mode remains non-activating
- **WHEN** a deploy run requests both dry activation and boot mode
- **THEN** the run builds and copies its selected profiles and carries boot
  intent without performing either live-switch or boot activation

### Requirement: Target-owned live-switch preflight
After copying a selected NixOS profile and before changing its profile or running any activation, the deploy engine SHALL invoke the copied generation's non-mutating pre-switch check when that generation advertises the switch-inhibitor contract. The target generation's result SHALL be authoritative: success selects the existing live switch path, while refusal selects boot-only staging. Mandala MUST NOT pass or preserve `NIXOS_NO_CHECK=1` into the preflight or live switch, and SHALL preserve the check diagnostic in the host event stream.

#### Scenario: safe generation keeps live activation
- **WHEN** a switch-mode generation advertises the check contract and its pre-switch check succeeds
- **THEN** Mandala runs the existing deploy-rs switch, wait, confirm, and per-host magic-rollback flow

#### Scenario: inhibitor refusal selects boot staging
- **WHEN** a switch-mode generation's pre-switch check refuses a critical change such as a D-Bus implementation transition
- **THEN** Mandala runs no live activation and selects the boot-only staging path with the inhibitor diagnostic retained

#### Scenario: ambient bypass is removed
- **WHEN** the invoking or remote environment contains `NIXOS_NO_CHECK=1`
- **THEN** Mandala's preflight and live switch do not inherit that bypass and the target check still governs the disposition

#### Scenario: older closure retains compatibility behavior
- **WHEN** the copied generation does not advertise the switch-inhibitor/check contract
- **THEN** Mandala preserves the member's existing static switch behavior rather than treating an unsupported check action as a reboot requirement

#### Scenario: dry activation previews the disposition
- **WHEN** a dry activation targets a generation whose check would refuse live switching
- **THEN** Mandala reports that a real deployment would be reboot-pending but changes neither the target profile nor its boot default

### Requirement: Transactional boot-only staging
The deploy engine SHALL stage inhibitor-refused and explicitly boot-mode generations as the next boot default without live activation. Before mutation it MUST record a recoverable previous system-profile target; success SHALL update the system profile and boot default to the new generation. If staging fails, Mandala SHALL restore the previous profile and previous boot default using boot-only operations and MUST NOT invoke `switch`, `test`, deploy-rs deactivation, or live reactivation for either generation.

#### Scenario: refused generation is staged safely
- **WHEN** the target pre-switch check refuses live activation and boot staging succeeds
- **THEN** the new generation becomes the system profile and boot default, the running generation is unchanged, and the host finishes `reboot-pending`

#### Scenario: explicit boot mode uses the safe transaction
- **WHEN** a member declares `activation = "boot"`
- **THEN** Mandala bypasses live-switch preflight and stages the generation through the same boot-only transaction

#### Scenario: boot staging failure restores the previous default
- **WHEN** installing the new boot default fails after the system profile has moved
- **THEN** Mandala restores the previous profile and previous boot default using only boot actions, reports the host failed with primary and recovery diagnostics, and performs no live activation

#### Scenario: recovery prerequisite is unavailable
- **WHEN** Mandala cannot resolve the previous system-profile target before staging
- **THEN** it fails the host before changing the profile or boot default

### Requirement: Optional cache publish selection
The native deploy engine SHALL resolve cache publishing once before run registration and build effects. In auto mode a nonempty `MANDALA_CACHE_URL` SHALL enable publishing; absent or empty URL SHALL preserve the existing no-publish behavior. `MANDALA_CACHE_NETRC_FILE` SHALL be optional upload authentication and SHALL NOT independently enable publishing. `--cache-push` SHALL force enablement and require a URL; `--no-cache-push` SHALL disable publishing even when the environment configures it. Both flags together SHALL be a usage error. Explicit disable SHALL ignore upload configuration. Enabled malformed URLs SHALL be rejected at preflight. CLI, TUI and MCP native launches SHALL preserve the same auto/enable/disable semantics and their existing mutation confirmation gates.

#### Scenario: no cache configuration
- **WHEN** neither override is supplied and the cache URL is absent or empty
- **THEN** the engine launches no cache copy and preserves existing build, host copy, activation and settlement behavior

#### Scenario: URL enables publishing without netrc
- **WHEN** auto mode receives a nonempty cache URL and no netrc file
- **THEN** publishing is enabled without adding a netrc option

#### Scenario: netrc alone is not a destination
- **WHEN** auto mode receives only `MANDALA_CACHE_NETRC_FILE`
- **THEN** publishing stays disabled

#### Scenario: disable overrides environment
- **WHEN** `--no-cache-push` is supplied with cache environment variables, including invalid values
- **THEN** no publish process starts and upload configuration does not fail preflight

#### Scenario: force requires a URL
- **WHEN** `--cache-push` is supplied without a nonempty cache URL
- **THEN** the engine refuses before registration or build effects

#### Scenario: contradictory flags are rejected
- **WHEN** both `--cache-push` and `--no-cache-push` are supplied in either order
- **THEN** argument validation fails rather than silently choosing one

#### Scenario: frontend omission preserves auto detection
- **WHEN** a native deploy is launched by CLI, TUI or MCP without an explicit publish override
- **THEN** the engine resolves its environment using the same auto policy

### Requirement: Publish the verified batch union before fan-out
When enabled, the engine SHALL publish the stable deduplicated union of verified successful `BuiltProfiles.paths` in one `nix copy --to <cache>` process after the single batch build and any effect-free recovery, before any per-host fan-out. It SHALL pass realized store paths, without another flake evaluation or build. Nix SHALL handle closure traversal and skipping paths already present. The engine SHALL NOT publish failed or unverified outputs, perform per-host uploads, or move publish effects into the recovery probe. No successful paths SHALL mean no publish process. Fatal resolution/build errors, halt-on-build-failure and interrupted builds SHALL prevent publishing and deployment as before.

#### Scenario: shared profiles publish once
- **WHEN** several successful hosts share a profile and another uses a distinct profile
- **THEN** one copy process receives each unique profile path exactly once in stable sorted order
- **AND** every host task starts only after that process has settled

#### Scenario: existing cache paths need no special probe
- **WHEN** some union paths already exist in the destination cache
- **THEN** the same single copy is used and Nix skips paths it already has

#### Scenario: partial build recovers successful outputs
- **WHEN** the batch build fails ordinarily and effect-free recovery verifies only some profiles
- **THEN** the upload contains only those successful paths and precedes their fan-out
- **AND** original build-failed hosts stay failed and the overall run remains nonzero

#### Scenario: no successful build outputs
- **WHEN** no selected profile is verified successfully
- **THEN** no cache copy or per-host deployment starts and existing build-failure outcomes remain visible

#### Scenario: build abort prevents publication
- **WHEN** resolution fails, a build is interrupted, or halt-on-build-failure aborts the build
- **THEN** no publish or host copy starts

### Requirement: Cache upload failure warns and falls back
An ordinary publish spawn failure or nonzero exit SHALL warn and record a failed publish outcome with a safe reason and numeric process result when available, then continue the existing per-host SSH copy for eligible profiles. Publish failure alone SHALL NOT fail the run, change host terminal outcomes, or retry automatically. Publish success SHALL NOT skip per-host copy. The existing `--substitute-on-destination` behavior SHALL remain enabled unless the host's resolved `fastConnection` is true; neither success nor failure SHALL override it.

#### Scenario: upload fails but hosts deploy
- **WHEN** cache copy exits nonzero and all eligible host deployments succeed
- **THEN** the publish warning and reason persist while the run succeeds absent any other build/controller/host failure
- **AND** the existing SSH copy path runs for every eligible host

#### Scenario: upload cannot spawn
- **WHEN** the publish process cannot start
- **THEN** a safe spawn-failed reason is recorded and host deployment continues without an automatic upload retry

#### Scenario: upload succeeds but target cannot substitute
- **WHEN** publish succeeds but a host cannot obtain a required path from its configured caches
- **THEN** its normal SSH copy still transfers missing paths and its normal copy result governs further deployment

#### Scenario: fast connection remains authoritative
- **WHEN** publishing is enabled and a host has `fastConnection = true`
- **THEN** that host still omits destination substitution and uses its existing direct-copy settings

#### Scenario: host failure still fails the run
- **WHEN** publishing fails and a host later fails or rolls back
- **THEN** the run is unsuccessful because of its host outcome, not because of the advisory upload failure

### Requirement: Publishing preserves credential and cache trust boundaries
The engine SHALL remain credential-agnostic: it SHALL NOT access secret stores or read netrc contents. It SHALL pass a configured netrc path only to the upload Nix process as `--option netrc-file <path>` and SHALL NOT expose it to unrelated subprocesses, per-host copies, metadata, event logs or diagnostic argv. Recorded cache URLs SHALL omit userinfo, query and fragment; executable argv MAY retain the configured destination in memory. Publish stdout/stderr SHALL NOT be forwarded raw into logs or observers; warnings and events SHALL use safe classified reasons and process status. The engine SHALL NOT add signing keys, sign profiles, relax upload signature checks, or change host substituters. The cache SHALL re-sign with a key hosts already trust; existing host configuration remains the operator's prerequisite.

#### Scenario: authenticated upload has a narrow boundary
- **WHEN** a netrc file is configured
- **THEN** only the upload child receives its path as the Nix option, the engine does not open the file, and unrelated child environments do not inherit `MANDALA_CACHE_NETRC_FILE`

#### Scenario: sensitive diagnostics stay redacted
- **WHEN** a configured URL contains userinfo or sensitive query data and the upload fails while printing sensitive output
- **THEN** terminal output, run metadata, event streams, diagnostic argv, TUI and MCP responses contain neither that sensitive URL data, the netrc path, nor raw upload output
- **AND** the safe warning reason and numeric exit status remain observable

#### Scenario: invalid URL is reported safely
- **WHEN** an enabled URL cannot be parsed safely
- **THEN** preflight reports an invalid cache URL without echoing its raw value

#### Scenario: existing trust is required
- **WHEN** publishing succeeds
- **THEN** no host trusted keys or substituters change and hosts can substitute only under their pre-existing trust/configuration

### Requirement: Publish process follows native termination ownership
The upload process SHALL follow the owned process-group, spawn/register, wait/deregister, guard cleanup and signal escalation contract in `docs/deploy-termination.md`. Termination SHALL NOT be converted into ordinary advisory failure: after termination begins, no publish retry, recovery or host fan-out effect SHALL start and the run SHALL exit nonzero. Completed/reaped process-group identifiers SHALL NOT be signalled on drop.

#### Scenario: terminate during publication
- **WHEN** SIGINT or SIGTERM arrives while the upload is active
- **THEN** the signal is forwarded to its owned group, the existing two-second grace and escalation policy applies, and the run settles unsuccessfully without fallback SSH

#### Scenario: publication was already reaped
- **WHEN** a completed publish operation is dropped after its process has been reaped
- **THEN** no signal is sent to its now-reusable process-group identifier

### Requirement: Publish milestone is durable and shared across frontends
The engine SHALL record a shared non-terminal `publish` milestone in an additive versioned `publish.jsonl` stream with plugin `publish`, an empty host tag and additive `actor: controller` attribution (so older readers do not invent a controller host), and persist an atomic additive `publish` metadata object with enablement, redacted destination when configured, outcome, unique path count and safe reason/process status when applicable. Outcomes SHALL distinguish pending, running, succeeded, failed, skipped and interrupted. Disabled, empty-output and build-not-completed skips SHALL be distinguishable. Eligible hosts SHALL receive the shared publish outcome as a non-terminal milestone before copy; it SHALL NOT become a host failure or an extra targeted host. CLI status, owned and attached TUI runs, and MCP `deploy_status` SHALL expose `cache-publish` while active and its outcome/warnings afterward without changing host counts, terminal states or effective run-result rules. Readers SHALL tolerate older records lacking publish data and ignore unknown additive data under the existing format contract.

#### Scenario: attach during shared publish
- **WHEN** a frontend attaches after build completion while the upload is still running
- **THEN** it reconstructs cache-publish and the redacted destination from durable state without claiming host copy or activation has begun

#### Scenario: attach after warning and fallback
- **WHEN** a CLI, TUI or MCP observer reads a settled run whose upload failed but hosts confirmed
- **THEN** it shows the advisory publish failure and reason alongside the successful host outcomes and effective run result

#### Scenario: host milestone is not host completion
- **WHEN** a host receives a publish milestone with failed shared outcome before copy
- **THEN** it remains eligible for copy and activation and is not marked failed or confirmed by that milestone

#### Scenario: legacy record or reader
- **WHEN** a new observer reads an older run with no publish object, or an older observer encounters the new stream, field or milestone
- **THEN** supported data remains readable without migration and existing sticky host outcomes keep their precedence

### Requirement: Publish preserves dry activation and boot behavior
Enabled publishing SHALL run for `--dry-activate` because that mode already builds and copies profiles, while still suppressing live and boot activation. Run-level `--boot` and per-member boot mode SHALL use the same publication phase and preserve reboot-pending semantics. Internal Nix dry-run resolution and any plan-only operation SHALL NOT publish; no new general dry-run option is implied.

#### Scenario: dry activation exercises distribution
- **WHEN** a run requests dry activation with publishing enabled
- **THEN** it builds, publishes and copies profiles but performs neither live nor boot activation

#### Scenario: boot run publishes before staging
- **WHEN** a run-level or member-level boot deployment has publishing enabled
- **THEN** the shared upload precedes host copies and successful staging still finishes reboot-pending rather than live-confirmed

#### Scenario: resolution probe is effect-free with respect to upload
- **WHEN** the engine runs the internal Nix dry-run profile resolution probe
- **THEN** no upload starts until the real batch build and any successful-output recovery have completed

### Requirement: Publish behavior has effect-isolated regression gates
Tests SHALL use stub Nix programs and temporary registry state to assert exact upload argv, deduplicated ordering, one-copy semantics, recovery ordering, empty/fatal-build skips, advisory fallback, flag/environment precedence, cross-frontend projection, credential redaction and process-group termination. Serialization and reader tests SHALL pin the additive metadata and milestone shapes and legacy compatibility. Tests SHALL NOT require live caches, credentials or fleet processes.

#### Scenario: publish contract regresses
- **WHEN** an implementation duplicates an upload, publishes unverified paths, starts fan-out early, fails a run solely for ordinary upload failure, leaks upload credentials or starts effects after termination
- **THEN** an isolated in-tree regression test fails before release

### Requirement: Native host copies report measurements
Every attempted native host copy SHALL durably record whether substitute-on-destination is enabled from resolved fastConnection, elapsed copy duration, and Nix-reported completed paths and transferred NAR bytes when available. Reporting SHALL NOT alter copy behavior or defaults. Human logs SHALL remain readable.

#### Scenario: destination substitution enabled
- **WHEN** resolved fastConnection is false
- **THEN** the copy event reports substitute_on_destination true and measurements describe controller-to-destination transfer only

#### Scenario: unavailable measurements
- **WHEN** Nix reports no recognized progress
- **THEN** counts remain absent rather than fabricated zero

#### Scenario: failed copy
- **WHEN** Nix cannot start or returns failure
- **THEN** mode and elapsed duration are recorded with failure and any reported partial counts, without proceeding to activation

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
Campaign reboot SHALL use exact eligible reboot-pending members, fresh evidence that the installed profile equals the bound system target, existing wrapper/order/drain controls and normal confirmation, subject to whole-request refusal when any eligible VM owner is included until a serialized owner reboot/reconnect transaction is separately qualified. Final verification SHALL compare fresh observations to bound system paths, including prior successes, without activation. Reboot command success or administrative close SHALL NOT imply final verification. Close SHALL refuse live or uncertain mutation, retain incomplete results and prevent further campaign mutation.

#### Scenario: staged members await proof of boot
- **WHEN** the installed profile equals the target but current differs
- **THEN** staging is successful and reboot-pending but final verification is incomplete

#### Scenario: close records partial completion
- **WHEN** an inactive campaign is closed with failures or exclusions remaining
- **THEN** those outcomes and reasons remain visible and close makes no success claim

### Requirement: Guest selection previews the owner application boundary
Native deployment SHALL resolve canonical selectors once, retain selected member/guest IDs, expand VMs to owner units and deduplicate owners. Before mutation approval it SHALL show owners, backend, requested guests, all potentially affected siblings, immutable desired references and readiness/recovery limits. Approval SHALL bind that plan and revision across CLI/TUI/MCP; changed impact SHALL require new approval. Explicit exclusion of a required owner or affected sibling SHALL be rejected as an impact conflict. Raw static batches SHALL not silently expand guest-only groups.

#### Scenario: two guests resolve to one unit
- **WHEN** two selected VMs share an owner, also directly selected
- **THEN** one application unit is previewed and built, with requested guests distinct from other affected siblings

#### Scenario: exclusion conflicts with shared application
- **WHEN** a selector selects a guest but excludes its required owner or an affected sibling
- **THEN** deployment refuses the conflict before approval and requires a revised selection

#### Scenario: adapter is unavailable
- **WHEN** a selected VM's owner has no supported host-application adapter
- **THEN** deployment refuses before build or transport effects and never falls back to guest SSH

#### Scenario: approval becomes stale before native child dispatch
- **WHEN** source inventory content, owner impact, settings or mode changes after a frontend previews a VM unit, including disappearance of VM impact
- **THEN** native dispatch refuses the captured approval before registration/build and requires a new shared preview

### Requirement: Frozen campaigns retain complete owner units
VM campaigns SHALL bind version-2 immutable owner-unit targets while preserving version-1 non-VM records. Requested guests and affected siblings SHALL remain distinct, but build/activation/retry/confirmation SHALL operate on complete owner units. Pure live preflight SHALL only reject invalid capture requests before archive/root effects; frozen inventory SHALL remain authoritative and repeated validation SHALL refuse races without a published campaign. Successful independent units SHALL remain untouched by retries, and unknown guest evidence SHALL block replay.

#### Scenario: an owner dependency fails but an independent owner succeeds
- **WHEN** a frozen VM owner build fails while an independent owner confirms
- **THEN** retry builds/copies/activates only the failed owner with its original requested guest identities and captured paths/settings

#### Scenario: frozen inventory rejects a live preflight result
- **WHEN** the reject-only live preflight passes but frozen ownership/exclusions conflict after source movement
- **THEN** temporary roots are cleaned and no campaign, profile build or host action is published

### Requirement: Unserialized VM campaign reboot is refused
Until a separately qualified serialized owner reboot/reconnect transaction exists, a campaign reboot request containing any eligible VM owner SHALL be wholly refused before survey/wrapper/run allocation. The refusal SHALL name VM owners and request explicit exclusion for ordinary-only dispatch. Ordinary-host reboots and read-only verification SHALL remain available. Owner surveys and reboot exit status SHALL NOT replace missing guest runtime evidence.

#### Scenario: mixed reboot request includes a VM owner
- **WHEN** eligible reboot targets mix a VM owner and ordinary hosts
- **THEN** the whole request is refused without partial dispatch, VM owners remain pending/guest-evidence-missing and read-only verify remains permitted

### Requirement: Single module-managed backend applies prebuilt guests through their owner
The initial `nixos-module` backend SHALL support only explicitly qualified NixOS owner/NixOS guest pairs with an injected owner module. It SHALL expose a versioned typed manifest and bounded inspect, prepare, apply, verify and restore operations. Evidence SHALL identify owner and guest, runtime instance, desired/previous/observed immutable generation or image, readiness contract/results, supported systems and per-guest dispositions. All transfer and runtime control SHALL use the owner's flattened transport and privilege settings, never guest SSH or guest-exec.

#### Scenario: module restarts a prebuilt guest
- **WHEN** the owner artifact and its referenced guest closure/image are available
- **THEN** the owner module applies or restarts the guest from those paths without building, re-evaluating or dialling the guest

#### Scenario: runtime identity is not observable
- **WHEN** a backend can report only its configured image pointer or host profile, not the running guest generation/image
- **THEN** conformance fails and the backend cannot claim guest deployment support

### Requirement: Guest deployment requires running identity and readiness
A guest SHALL be deployed/confirmed only after its requested generation/image is observed running and bounded declared readiness checks pass. Staged images and boot-only host staging SHALL leave the guest pending, not deployed. A failed live restart or readiness deadline SHALL fail rather than be relabelled pending. Host target-owned preflight, boot-only staging and dry-activation guarantees SHALL remain authoritative.

#### Scenario: matching identity is not enough
- **WHEN** the desired image is running but readiness fails
- **THEN** the guest is not confirmed and owner recovery policy is invoked

#### Scenario: live-switch inhibitor refuses
- **WHEN** the owner pre-switch check refuses live activation
- **THEN** only transactional boot staging is allowed, guest runtime is not restarted to bypass the inhibitor, and unapplied guest changes remain pending

#### Scenario: dry activation
- **WHEN** a VM owner unit is dry-activated
- **THEN** build/copy may occur but profiles, runtime and image pointers do not change, and guests are preview/not-applied rather than deployed

### Requirement: Owner-scoped runtime recovery is verified
Before mutation the backend SHALL durably record and retain previous owner profile/boot state and every affected guest runtime/image plus readiness/recovery contract. Missing recoverable prior runtime SHALL refuse mutation in the first backend. With automatic rollback enabled, application or verification failure SHALL restore and verify affected owner state and every previous guest runtime/readiness before reporting rolled-back. With automatic rollback disabled it SHALL record failed/unrecovered state. Primary and recovery failures SHALL remain separately visible; writable data SHALL NOT be claimed restored.

#### Scenario: profile rollback alone
- **WHEN** deploy-rs restores the owner profile but a guest still runs the new image
- **THEN** the unit is not rolled-back and requires guest runtime recovery

#### Scenario: only one sibling recovers
- **WHEN** one affected guest restores successfully but another fails restoration or verification
- **THEN** per-guest evidence is retained, the unit reports partial recovery and the run is unsuccessful without revoking independent successful owners

#### Scenario: no previous runtime exists
- **WHEN** initial guest creation has no recoverable prior runtime
- **THEN** the first backend refuses before mutation rather than promising nonexistent rollback

#### Scenario: interrupted verification
- **WHEN** interruption or timeout leaves runtime state unobserved
- **THEN** the run records unknown/recovery-required state, performs no detached recovery or prohibited new effects, and requires reconciliation before another owner mutation

### Requirement: VM application is qualified with effect-isolated fixtures
Fake-fleet and injected-effect tests SHALL gate ownership validation, projection guards, owner-plan preview/exclusions, one graph/build, immutable manifest dependencies, owner lock behavior, running identity/readiness, staged/dry modes, runtime restoration and partial/unknown recovery. Tests SHALL assert no guest SSH, guest-exec, activation-time evaluation or build, and no real fleet mutation.

#### Scenario: fake effect trace exposes a forbidden guest operation
- **WHEN** an implementation attempts a guest transport, second build or activation-time evaluation
- **THEN** the isolated conformance gate fails before implementation can ship

#### Scenario: concurrent runs and failed recovery are simulated
- **WHEN** barrier-controlled fake runtimes race sibling selections and inject restore/readiness failures
- **THEN** tests prove owner serialization and truthful individual and overall outcomes
