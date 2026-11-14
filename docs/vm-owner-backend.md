# Owner runtime backend

This describes the opt-in `nixos-module` backend contract. The native owner
transaction seam and its effect-isolated fixtures exist, but owner registration
and VM campaigns remain fail-closed until shared approval and frontend settlement
qualification is complete. This is not permission to enroll or deploy a VM.

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
generic success string is not evidence. The operations are:

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

## Qualification

`vm-owner-resolution`, `vm-owner-backend` and `vm-owner-module` checks use fictional
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
