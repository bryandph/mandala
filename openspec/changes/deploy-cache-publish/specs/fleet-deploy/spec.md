## ADDED Requirements

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
