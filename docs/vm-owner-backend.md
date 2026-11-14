# Owner runtime backend

This describes the opt-in `nixos-module` backend and its effect-isolated
qualification. Native owner deployment and frozen owner-unit campaigns require
exact current preview approval; unsupported/missing capabilities fail closed.
The helper is disabled by default. This is not permission to enroll or deploy a
VM, or qualification of a consumer's adapter.

## Optional host component

The `nixosModules.vm-owner` module installs `mandala-vm-owner` only when
`mandala.vmOwner.enable = true`. Consumers explicitly supply the Mandala package
and canonical `mandala.vmOwner.owner` member ID. With the option disabled, no
helper package, configuration file, state directory rule, listener or service is
contributed. A fictional authoring example, not an adoption instruction:

```nix
{
  imports = [ mandala.nixosModules.vm-owner ];
  mandala.vmOwner = {
    enable = true;
    owner = "host-a";
    package = mandala.packages.${pkgs.stdenv.hostPlatform.system}.mandala-rs;
  };
}
```

The helper reads its owner identity from `/etc/mandala/vm-owner.json` and refuses
another owner's ID, preventing an alternate lock-directory name from bypassing
serialization on the same host. Its private state is under
`/var/lib/mandala/vm/<owner>`. It is neither a daemon nor a network service: the
controller starts a bounded SSH stdio process with the existing owner endpoint,
SSH options, identity, port and flattened privilege settings. There is never a
guest endpoint, guest SSH task or foreign guest switch binary.

## Artifact and consumer adapter

The consumer's injected module supplies the runtime, not Mandala. No runtime or
virtualization flake is pinned. It must place the versioned JSON manifest at
`mandala/vm-application.json` inside the existing owner application profile.
`host.vmApplication` remains the pure compatibility summary described in
[VM members](vm-members.md); it is not a free artifact/installable override.

The exact typed manifest and observation definitions live in
`crates/mandala-core/src/vm/manifest.rs`. The owner generation, adapter executable,
all guest closures/images and readiness/recovery contracts are immutable store
references in the same owner artifact closure. Guest generation identities are
store paths; immutable image identities may instead be lowercase `sha256:`
digests. Mutable aliases such as `current` or `latest` are not runtime identities.
The consumer builds these references in its existing single configuration graph,
without asking a guest configuration to import the owner configuration.

Both controller and helper validate exact relation/manifest agreement and the
complete closure. After copy, the helper compares the actual stored manifest
with the approved one. A failed sibling dependency cannot yield an eligible
owner artifact. Runtime application does not evaluate flakes, run a second build,
construct an image, or invoke `nixos-rebuild`.

The immutable adapter is called with `--mandala-vm-protocol=1`, one JSON
`AdapterRequest` on stdin, and one typed `AdapterReply` on stdout. Definitions in
`crates/mandala-core/src/vm/owner.rs` are authoritative. A zero process exit or a
generic success string is not evidence. `AdapterRequest.profile` identifies the
prepared deploy profile (which may wrap a NixOS toplevel); `ownerProfile` must
match it, while `ownerRunning` must match `manifest.ownerGeneration`, not the
wrapper. The operations are:

- **inspect:** owner profile, running generation and boot default; every affected
  guest's actually running identity, runtime instance, immutable artifact,
  readiness and previous readiness/recovery contracts;
- **prepare:** complete available guest paths, qualified systems and exact runtime
  instances. Prior owner state and every prior guest runtime must be recoverable;
  initial creation without that evidence is refused;
- **apply:** bounded live restart/prebuilt-image selection, or explicit pending
  staging for boot intent. Boot staging must not restart guests;
- **verify:** independently observed owner/guest identities and readiness, with an
  explicit desired-versus-previous verification target;
- **restore:** restore prior owner/runtime state, followed by independent verify.
  A restore exit or host profile rollback alone never proves runtime recovery.

Readiness bounds come from the manifest. A live restart deadline is failure,
not pending. Unsupported observation shapes and protocol versions fail closed.

## Transaction and recovery protocol

The controller and helper exchange newline-delimited, bounded JSON messages.
The first request is `{"op":"hello","version":1}`; either-direction version
mismatch names controller and helper versions and refuses before owner effects.
The protocol types define the remaining exact request/reply shapes.

The controller acquires the owner OS file lock **before copy**. The session
holds it across copy, prepare, target-owned preflight, deploy-rs activation,
runtime apply/verify and any recovery. Independent owners have separate locks;
a busy owner has a bounded visible refusal, never implicit lock stealing.

Before host mutation, the helper atomically writes and fsyncs the previous owner
profile/running generation/boot default, every affected guest runtime and its
contracts, desired manifest, owner plan, and recovery limitations. It registers
store GC roots for desired and previous paths. The controller may invoke host
activation only after the helper acknowledges durable mutation intent. Rooting
or journal failure prevents mutation; a persistence failure poisons the session
so a subsequent request cannot bypass it.

Live-switch inhibitor refusal preserves the existing transactional host boot
staging path. The adapter receives boot mode, verification requires unchanged
running guest identities, and guest outcomes remain pending. Dry activation may
copy and inspect/prepare but cannot start host/runtime mutation, write the owner
recovery journal, or root previous runtime state; guest outcomes are preview.

Known failures before mutation may be explicitly aborted as not-applied.
Otherwise EOF, deadline, cancellation or process death leaves unknown/unsettled
state. No detached recovery runs, no unverified operation is automatically
replayed, and a later normal transaction is blocked. An explicit reconciliation
session must acquire the same lock and independently verify desired state or
restore and verify previous state. A crashed mutation intent is normalized to
unknown on admission. Journals without enough previous evidence require
operator investigation rather than invented recovery.

Automatic rollback policy extends host rollback with guest restore and verify.
Disabled rollback records failed/unrecovered. A restored sibling and a failed
sibling remain distinct; partial or unknown recovery is unsuccessful. Primary
and recovery diagnostics are independent. Known failed outcomes may close their
session, but still block new mutation until explicitly reconciled.

Writable guest disks, application data and external services are **not** reverted
by image/generation restoration. Desired and previous roots are conservatively
retained; they must not be removed while recovery/reconciliation is outstanding.

## Approval, attachment and campaigns

CLI `--preview`, native TUI confirmation and MCP refusal previews share the same
owner plan. The fingerprint binds evaluated inventory content, complete impact,
flattened settings, targets and boot/dry mode. Native CLI requires
`--approve-owner-plan`; MCP requires `owner_approval` even for dry activation.
The supervised child re-evaluates and rechecks the captured fingerprint before
registration/build. A changed or disappeared owner unit requires renewed review.
Native TUI refuses confirmation if any part of its complete preview is clipped;
resize or use CLI/MCP for a large plan. This content identity is an inventory
approval binding, not native source freezing; use a campaign for frozen builds.

VM-only registry fields retain the approved plan, revision, fingerprint and
context. Shared read-only reconstruction validates that binding before judging
`vm/<owner>.json` and owner-attributed VM events. Missing/corrupt evidence,
unverified rollback or partial/unknown recovery cannot be hidden by rc 0 or host
confirmation. Attaching never evaluates or contacts a host. CLI, TUI and MCP
share guest counts, actual identity/readiness and primary/recovery diagnostics;
guests do not become independent transport hosts.

A VM campaign writes schema version 2 with additive `target.vm_owner_plan`.
Existing non-VM campaigns retain version-1 target digests/bytes and remain
readable. A pure live inventory preflight only rejects invalid selectors,
ownership or exclusions before archive/roots; none of its data becomes a target.
Authoritative frozen resolution repeats validation; a race/refusal drops all
temporary roots without publishing a campaign. Complete affected owner units
are bound, preserving requested guest identities and sibling impact. Guest
retry selectors map to their bound owner, with exact owner-unit confirmation
including the frozen plan fingerprint. Successful independent units are never
built/copied/activated again. Unknown guest evidence blocks campaign replay.

Owner-only surveys and reboot success cannot qualify guest runtime. Campaign
views explicitly say `guest evidence missing` and stay incomplete until durable
verified runtime evidence matches the bound unit, profile and system generation.
A serialized owner reboot/reconnect transaction is deferred. Campaign reboot
refuses the whole eligible request if any VM owner is included, naming those
owners and asking for explicit exclusion to reboot ordinary hosts. No wrapper,
survey or run is allocated by that refusal; no partial dispatch occurs. Existing
ordinary-only reboot behavior and read-only verify remain available. VM owners
stay reboot-pending/guest-evidence-missing, not implicitly verified.

## Qualification

`vm-owner-resolution`, `vm-owner-backend`, `vm-owner-artifacts` and
`vm-owner-module` checks use fictional
fleets, injected runtime/store effects and local stub nix/SSH programs only.
They do not require a real owner, guest or network service. Backend checks cover
cross-session lock contention, independent owners, protocol mismatches, persistence
and rooting failures, first creation refusal, immutable identity/readiness
mismatches, live deadlines, boot/dry dispositions, disabled/partial recovery,
abrupt crash admission and explicit reconciliation. The native seam's trace
pins lock-before-copy and host activation before guest apply/verify, without a
second build/eval or independent guest transport.

macOS owners remain unsupported. Package installation, a compatibility summary,
or these fixtures do not qualify a consumer adapter or authorize real adoption.

### Recorded qualification limits

The implementation checkpoint was qualified on `x86_64-linux`:

- Sandboxed `nix build .#mandala-rs`: **597 passed, 0 failed, 19 intentionally
  ignored doctests**, across 43 workspace test binaries/doctest suites. This is
  the authoritative package check, including transport-dependent context tests.
- Focused `vm-owner-resolution`: 11 Rust tests; `vm-owner-backend`: 44 Rust tests
  across core/deploy/MCP/TUI, including the MCP launch-boundary integration test.
- VM schema/projections, owner module/artifact graph, all seven builder checks
  and fake-fleet checks passed. Non-VM baseline fixture bytes were unchanged.
- Scoped all-target clippy (`mandala-core`, `mandala-deploy`, `mandala-mcp`,
  `mandala-tui`, `mandala`, `--no-deps -- -D warnings`) and formatting passed.
- Every executable CI step passed: synthetic controller identity evaluation,
  root flake check, formatting, fleet template check, showcase check,
  showcase `network-tf` rendering and the Rust package build. No lock/input or
  workflow files were changed.
- Host `nix develop -c cargo test --workspace --no-fail-fast` ran all suites:
  **567 passed, 30 failed, 19 ignored**. Failures are transport/`LeaderBusy`
  limitations across the existing stdio/context/proxy/TUI-context targets;
  all corresponding tests pass in the authoritative sandboxed package. This
  is not a claim that the unsandboxed command was green. A fresh untouched
  `git archive` of baseline `63373b1d8ad0b2437df035665cc81d3a48bdad98`, using
  the exact same command/environment, reproduces all 30 failed test names and
  panic/error diagnostics (ignoring PIDs, source line shifts and once-per-process
  backtrace-hint placement). All 312 archived tracked blobs still match Git
  after execution. None of these branch failures passes on the baseline:

  | Package / integration target | Baseline failures | Branch failures |
  | --- | ---: | ---: |
  | `mandala / mcp_stdio` | 3 | 3 |
  | `mandala-context / context` | 3 | 3 |
  | `mandala-context / failover` | 7 | 7 |
  | `mandala-context / lifecycle` | 7 | 7 |
  | `mandala-mcp / context_proxy` | 1 | 1 |
  | `mandala-mcp / mcp_failover` | 1 | 1 |
  | `mandala-mcp / parity_proxy` | 1 | 1 |
  | `mandala-tui / context_flow` | 7 | 7 |
  | **Total** | **30** | **30** |
- OpenSpec CLI was unavailable, so strict CLI validation was **not run**.
  Local delta/scenario structure and coverage review covered four delta files,
  25 requirements and 72 scenarios; this does not substitute for the CLI.
- No ARM or Darwin execution was tested. Pure synthetic Darwin identity parity
  is not a runtime/backend support claim. No consumer, owner or live guest was
  contacted or enrolled; all new backend effects use local fixtures/stubs.
