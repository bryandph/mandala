## Context

`build_profiles_with` in `crates/mandala-core/src/engines/deploy.rs` resolves the selection before the single batch build. On an ordinary best-effort build failure, `recover_profiles` probes exact outputs with building and substitution disabled and returns verified successes in `BuiltProfiles.paths`, alongside sticky host failures. `fan_out_with` schedules only successful hosts. Publishing must consume that result, not installables, derivations, or an independently evaluated selection.

`push_profile` in `crates/mandala-deploy/src/push.rs` already passes `--substitute-on-destination` unless the resolved host setting `fastConnection` is true. Publishing supplies missing locally built paths to an existing cache; it does not create a new host-copy algorithm.

The durable protocol and compatibility gates live in `openspec/specs/fleet-state-formats/spec.md`; frontend behavior lives in `openspec/specs/fleet-cli/spec.md`. Local signal forwarding and process-group ownership are documented in `docs/deploy-termination.md`. Those contracts continue to apply.

## Goals and Non-goals

Publish verified profiles once before host fan-out, allow explicit operator control, preserve direct-copy fallback, and make the shared phase observable without leaking upload credentials or confusing it with a host failure.

Non-goals are consumer credential acquisition, builder publishing, cache lifecycle, per-host cache selection, host trust configuration, and changes to activation or rollback.

## Decisions

### 1. Resolve explicit overrides before environment detection

Use a tri-state per-run choice (`auto`, `enable`, `disable`), resolved once in the engine before run registration and build effects. The CLI exposes `--cache-push` as enable and `--no-cache-push` as disable. Supplying both is a usage error, not last-argument-wins. Frontend launchers must preserve that tri-state instead of turning absence into false.

| Choice | Nonempty `MANDALA_CACHE_URL` | Result |
| --- | --- | --- |
| disable | either | Disabled; ignore upload configuration, including invalid values |
| enable | absent or empty | Preflight error before registration/build |
| enable | present | Enabled |
| auto | absent or empty | Disabled; existing behavior |
| auto | present | Enabled |

A nonempty `MANDALA_CACHE_NETRC_FILE` is optional authentication, not an independent enable signal. Netrc alone with no URL in auto mode leaves publishing disabled. Empty variables count as absent; nonempty values are not whitespace-trimmed into different URLs or paths. Validate the enabled URL as a Nix store URL without limiting it to HTTP; reject malformed configuration before registration. Nix decides backend availability and upload success at execution time. Do not validate or open the netrc file in the engine; unreadable credentials are an ordinary publish failure reported by Nix.

**Alternative:** require both environment variables to enable publishing. Rejected because unauthenticated writable stores are valid and a URL is the destination, while netrc is only one optional upload authentication mechanism. **Alternative:** silently let one conflicting flag win. Rejected because a contradictory invocation should not unexpectedly upload.

Apply this resolution to every native deploy launch (headless CLI, standalone/explorer TUI, and MCP). Any typed frontend option carries the same tri-state; omission is auto. The engine's environment is the configuration source, including environment supplied by an existing controller wrapper. No frontend reads a secret store or bypasses existing mutation confirmation gates.

### 2. Publish only after verified build recovery, before all host work

The sequence is preflight and registration → publish run id → resolve profiles → one batch build → effect-free recovery if needed → optional publish → throttled per-host copy/preflight/activation → existing settlement.

Form a stable, lexicographically sorted set from `BuiltProfiles.paths` values. Keep its GC roots alive through publish and fan-out. Launch exactly one command for a nonempty enabled set, using argv entries rather than shell interpolation:

```text
nix copy --to <configured-store-url> [--option netrc-file <configured-netrc-path>] <unique-profile-path>...
```

The bracketed option is present only when configured. `https://cache.acme.example` is a fictional example destination. Pass realized store paths only; Nix transfers their transitive closures and skips already present paths. Do not enumerate each closure into separate copy commands, evaluate another flake, rebuild, retry a failed profile, add `--no-check-sigs` to the cache upload, or publish once per host.

On partial build failure, publish only verified successes, then retain the original failed hosts and nonzero run result. With no successful paths, skip the command and retain normal build failure settlement. A fatal resolution/build error, interrupted build, or halt-on-build-failure return never enters publishing. Publishing does not move inside recovery: its Nix store upload effects cannot be part of the effect-free probe.

**Alternative:** publish during per-host copy or build recovery. Rejected because it duplicates work or violates the probe's no-build/no-substitution boundary. **Alternative:** publish static `deployBatch` artifacts. Rejected because their selection and artifact shape are a separate cache-warming interface.

### 3. Ordinary upload failure is advisory; termination is not

Spawn failure and nonzero Nix exit produce a warning and a failed publish outcome with a safe reason and process rc when available. Do not retry automatically. Continue all eligible hosts through the existing `push_profile` path, with the same settings as a disabled-publish run. A partially populated cache may still help. Publish failure alone never changes host terminal outcomes or the overall run rc. Existing build/host/controller failures still govern settlement.

This fallback is the existing SSH copy, not a forced `fastConnection` override: destination substitution remains enabled by default and Nix copies the still-missing paths over SSH; `fastConnection = true` still omits destination substitution. Even a successful publish does not skip per-host copy or prove that a host can reach or trust the cache.

Termination/cancellation is distinct from an advisory upload failure. Follow the exact ownership policy in `docs/deploy-termination.md`: the publish child is a new process-group leader, spawn/register and wait/deregister share the signal-forwarding lock, unreaped guards clean up, stdout/stderr drain concurrently, and completed/reaped PGIDs are never signalled on drop. SIGINT/SIGTERM are forwarded, with the existing two-second grace then escalation. Once termination starts no fallback SSH, recovery, publish retry, or other new effect may start; the run exits nonzero. No publish subprocess is detached. Do not promise cleanup of descendants of a normally reaped leader beyond the documented ownership limits.

**Alternative:** fail the run on cache upload failure. Rejected because cache publishing is an optimization; per-host SSH remains the correctness path. Treating explicit termination as the same fallback case would violate the no-new-effects guarantee.

### 4. Keep credentials in the upload process boundary

The engine never acquires credentials, opens the netrc file, reads its contents, or persists its path. Pass `--option netrc-file <path>` only to the publish Nix child. Remove `MANDALA_CACHE_NETRC_FILE` from unrelated child environments (and from the upload child's environment once translated to the explicit option); frontend-to-engine handoff may carry it, but registry metadata and recorded launch argv must not. Do not put it in global Nix options, SSH options, per-host commands, or host state.

Separate executable arguments from diagnostic arguments. Records and terminal/UI/MCP output use a redacted URL with all userinfo, query, and fragment removed; the actual URL remains in memory for the copy only. Mask the netrc option value in any diagnostic argv. If a safe URL cannot be derived, use a redacted placeholder, never the raw input in an error. Do not log environment dumps. Capture and discard raw publish stdout/stderr rather than forwarding arbitrary backend messages that could echo credentials. Persist safe classified reasons (`spawn-failed`, `copy-exit`, `interrupted`, `no-built-paths`, `disabled`) and numeric exit status where available, not unfiltered backend errors or OS messages containing credential paths. Warnings identify the redacted destination and reason; detailed upload debugging belongs outside the durable deploy stream.

**Alternative:** stream Nix output unchanged and scrub known strings. Rejected because a credential-agnostic engine cannot know the contents of a netrc it must not read, and a backend may echo derived credentials. Classification plus exit status is the deliberate diagnostic boundary.

The cache re-signs uploaded paths using a key hosts already trust. Hosts must already have the corresponding substituter and trust configured; Mandala adds no key, changes no host substituter, and does not sign paths or relax upload signature checks. No cache URL, credential, or trust data is added to the fleet aggregate.

### 5. Add a shared publish milestone without a new host terminal state

Use an additive `publish.jsonl` stream through the existing versioned `EventWriter`, with plugin `publish`, an empty host tag and an additive `actor: controller` field. The empty host is deliberate: the existing `EventTailer::route` in `crates/mandala-core/src/runner.rs` treats any nonempty host on a non-build plugin as a fleet host, so a literal `controller` host would corrupt older readers' host counts. Updated readers route the publish plugin separately. Emit `milestone: publish` with additive outcome fields at phase start and completion, and safe warning lines on failure. Metadata gains an atomic `publish` object containing `enabled`, optional redacted `cache_url`, `outcome` (`pending`, `running`, `succeeded`, `failed`, `skipped`, or `interrupted`), `path_count`, optional `reason`, and optional `process_rc`. Disabled runs start skipped/disabled; enabled runs start pending. An empty verified set settles skipped/no-built-paths. A build failure preventing phase entry settles skipped with a safe build-not-completed reason. Interrupted runs remain unsuccessful even if the upload's process result is unavailable.

For each eligible built host, append a non-terminal `publish` milestone carrying the shared outcome before its existing `copy` milestone. This is attribution of one shared operation, not additional uploads. Build-failed hosts keep their sticky failure and receive no false publish success. Do not emit a nonzero host `status done` for an upload failure or map `publish` to a failed/confirmed host state.

Status, runs views, owned/attached TUI and MCP `deploy_status` project `cache-publish` while this phase runs, independently of host concurrency. They show the redacted destination, shared outcome and warning reason, and preserve existing host counts and terminal failure precedence. Publish completion is not host deployment completion. New observers reconstruct the phase from durable metadata/events, including when attaching mid-publish or after a warning-and-fallback run. Older records without the object mean no recorded publish phase, not an error; older readers ignore the additional fields/stream/milestone under the existing format rules. Pin new golden bytes and reader judgements; no existing event meaning or event version is changed. A registry I/O failure remains an existing controller infrastructure failure, not a Nix upload failure exempt from durability requirements.

**Alternative:** represent the controller as another fleet host or use its failed upload rc as the run rc. Rejected because that corrupts host totals and the advisory failure policy.

### 6. Preserve dry activation and boot semantics

`--dry-activate` is not an effect-free deployment: it already builds and copies. When publishing is enabled it also publishes before fan-out, and still performs no live or boot activation. `--boot` and per-member boot modes use the same publish phase; reboot-pending and rollback semantics remain unchanged. The internal Nix `--dry-run --json` resolution probe is not a publish trigger. No new general `--dry-run` option is introduced; any plan-only path performs no upload.

**Alternative:** suppress publishing for dry activation. Rejected because dry activation must exercise the same distribution/copy path while suppressing activation only.

## Delivery and Verification

Implement additive option/argv and safe-record types first, then the shared phase, then launch/observer wiring. Keep publishing opt-in at every step. Use stub Nix programs and temporary run directories for tests, never a live cache or fleet. Test exact argv, union ordering, the build/recovery/publish/fan-out barrier, partial builds, fallback, redaction and termination. Exercise CLI, TUI and MCP through the same engine policy and durable projections, with serialization compatibility gates.

The targeted existing checks are `nix build .#checks.<system>.fake-fleet` for contract preservation and `nix build .#packages.<system>.mandala-rs` for workspace tests, including the termination integrations. Run `nix fmt -- --check .` and `nix flake check` as final gates; `flake.nix` is authoritative for current target names. Implementation must extend `docs/deploy-termination.md` and relevant frontend/format specs when the behavior ships.

## Approval and Open Questions

The interface names, opt-in policy, advisory failure policy, single union upload, and existing host trust boundary are fixed inputs to this plan. The choices above settle edge cases against the current engine contracts; no blocking question remains. This documents proposed implementation, not an implemented feature. Artifact completeness is not approval: obtain plan review and explicit approval before implementation or consumer changes.
