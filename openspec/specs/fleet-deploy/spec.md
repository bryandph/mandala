# fleet-deploy Specification

## Purpose
Native fleet deployment: a selection evaluates and builds once, then activation fans out in parallel per host from the prebuilt profiles with per-host rollback, deploy-settings fidelity, target-owned live-switch preflight, and transactional boot-only staging. Static `deployBatch.<group>` outputs remain the cache-warming surface.

## Requirements
### Requirement: Eval-once batch deploy
The system SHALL evaluate a deploy selection once: settings resolve from the
versioned aggregate, exactly the selected per-host profile installables build
in one `nix build` invocation, and activation fans out from those prebuilt
profiles with NO further flake evaluation — no per-host `deploy` CLI re-evals
and no unrelated-member builds. Static `deployBatch.<group>` linkFarms remain
the cache-warming surface rather than the runtime selection API.

#### Scenario: a group deploy evaluates once
- **WHEN** a multi-host selection is deployed
- **THEN** the flake is evaluated a single time, profiles build once, and
  each host activates from its prebuilt profile without triggering another
  evaluation

#### Scenario: cache warming is unchanged
- **WHEN** `deployBatch.<group>` is built and pushed by the cache-warm app
- **THEN** the artifact shape and push flow are identical to the pre-engine
  behavior

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

#### Scenario: a bricked host rolls back
- **WHEN** activation on one host fails its confirm within the timeout
- **THEN** activate-rs restores that host's previous generation, and the run
  reports the rollback as that host's sticky terminal state

#### Scenario: a failure does not revoke siblings
- **WHEN** one host of a multi-host run fails and rolls back
- **THEN** hosts that already confirmed stay on the new generation, pending
  hosts continue, and the run summary reports the partial outcome loudly

### Requirement: Fleet deploy result includes host outcomes
The fleet deploy pipeline SHALL report an overall failure when the controller process exits non-zero or any targeted host reaches the sticky `failed` or `rolled-back` state. `confirmed` and `reboot-pending` SHALL both be successful terminal states, but summaries MUST count them separately and MUST NOT describe a reboot-pending host as live-confirmed. A zero controller exit MUST NOT convert a failed or rolled-back host outcome into overall success.

#### Scenario: mixed fan-out rolls one host back
- **WHEN** the controller process exits zero after one target confirms and another target rolls back
- **THEN** the overall deploy result is non-zero/failed while both per-host outcomes remain visible

#### Scenario: every host confirms
- **WHEN** the controller process exits zero and every reported target confirms
- **THEN** the overall deploy result is successful and may report that all hosts confirmed

#### Scenario: successful run includes staged hosts
- **WHEN** a run finishes with confirmed and reboot-pending hosts and no failed or rolled-back host
- **THEN** the overall deploy result is successful while the summary reports both counts and does not claim all hosts confirmed

### Requirement: Native parallel fan-out
The deploy engine SHALL orchestrate fan-out in-process: a throttled pool of
per-host deploy tasks, each writing its own attributed event stream. A
`--limit` SHALL remain required and resolve through the canonical taxonomy
spelling; selected members without `deployRs.enable` are skipped with an
explicit notice; a selection with zero deployable members is refused before
a run is created. An abnormal failure (including a panic) in one host's
task SHALL fail only that host.

#### Scenario: throttle bounds concurrency
- **WHEN** a selection larger than the throttle deploys
- **THEN** at most `--throttle` hosts are in flight at once and every host
  is eventually processed

#### Scenario: a non-deploy member is skipped loudly
- **WHEN** the selection includes a member with ansible management but
  `deployRs.enable = false`
- **THEN** the run proceeds for the deployable members and reports the
  skipped member by name

#### Scenario: a task crash is contained
- **WHEN** one host's deploy task fails abnormally mid-run
- **THEN** that host is reported failed and every other host's deploy
  completes normally

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
