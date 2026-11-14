## MODIFIED Requirements

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

## ADDED Requirements

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
