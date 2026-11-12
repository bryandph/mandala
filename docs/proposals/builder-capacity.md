# Builder capacity and controller projections

This proposal records the chosen design and rationale for projecting build
capacity from fleet members. It is not an implementation or rollout record.
The first-slice normative contract is the
[builder-capacity-projection OpenSpec change](../../openspec/changes/builder-capacity-projection/proposal.md).
Complete artifacts alone do not authorize implementation, credential changes,
consumer migration or deployment.

## Boundary and motivation

`host.build` describes how to build a member's closure. It does not say which
other derivations that member can execute. Operator identity is a third,
independent concern. Capacity belongs to a member; access and selection policy
belong to the controller that will use it.

Consider fictional acme and globex fleets. Operator-alice may use a Linux
controller under WSL, a macOS controller, a Linux builder VM hosted on the macOS
controller and reached on a localhost port, and an x86_64 workstation builder.
The same VM can require a jump from one controller and a direct connection from
another. Two Linux controllers can also have different permissions and keys.
An OS selector or opaque alias list cannot capture those distinctions reliably.

Existing authorities remain `schema/member.nix`, `schema/operator.nix`,
`lib/default.nix`, `flake-modules/fleet.nix` and `docs/convention.md`.
Examples here describe roles, not a real fleet baseline.

## Decision 1: Member-owned capacity, separate controller policy

Use nullable `host.builder`, with null meaning no offered capacity. Explicit
systems and feature sets describe execution capability; never infer emulation
from architecture or closure build instructions. Jobs and speed are positive
integers with conservative defaults of 1. Features default empty, and mandatory
features must be supported. Connection defaults include endpoint, builder login,
port, ssh-ng protocol, optional remote-program path and public trust. The builder
login is independent of deploy root/sudo settings.

A separate operator builders table was considered, but would duplicate inventory
identity and introduce competing sources. Operator identity is not the right
place for infrastructure capacity either.

Named lanes override scheduling fields and optionally systems. No lanes means
one default lane; a nonempty lane map replaces it. Null overrides inherit, while
explicit values replace, not append. Lists can remove inherited features.
Normalize sorted unique feature/system sets and sort members then lanes. Keep
distinct lanes even on one endpoint.

A wide lane and a one-job big-parallel lane are advertisements, not reservations.
They cannot promise exclusive memory against one another or another controller.
Qualify actual feature matching and shared-endpoint slot accounting on the
deployed Nix version before claiming the intended scheduling policy is effective.
Global budgets or exclusive memory need builder-side controls or separate work.

## Decision 2: External capacity is facts-only membership

Enroll a builder without a managed closure through existing
`extraMembers` / `evalMember`, with management surfaces disabled and host.build
null when appropriate. Give a hosted VM its own identity. Do not fabricate network
attachments, shadow existing members, or create a separate external registry.

Membership can change all-member selectors, inventory, DNS and documentation
views even without creating deployment targets. Review the complete projection
diff before adoption. A separate registry is a later possibility only if member
enrollment is intentionally forbidden, not a parallel source of truth now.

## Decision 3: Structured routes, strict trust, string-only keys

Each controller supplies structured member routes: endpoint/port, builder login,
absolute identity path, optional typed jump reference, optional remote-program
path, and trust override when the endpoint identity differs. Missing jump
references or cycles fail evaluation. A jump can be a facts-only non-builder
member, but still needs its own complete connection and public trust.

Use absolute strings for private-key paths, never Nix path literals, readFile,
key contents, invoking-user HOME or SSH-agent discovery. Pure evaluation does
not read keys; the consuming daemon must later be able to read them
noninteractively. Builder routes do not inherit deploy identity implicitly.

Require public host-key pins or an explicit public CA and a name/principal it
actually covers. Both jump and final endpoints need trust. No interactive
acceptance, runtime keyscan, or strict-checking bypass is permitted. Localhost
VMs need stable distinct HostKeyAlias/known-host identities and correct
nondefault-port matching. A knownHosts pin and nix.buildMachines.publicHostKey
have different representations: the latter encodes the full SSH public-key text,
not merely its base64 key blob.

Generate a canonical structured model for system-wide SSH/knownHosts adapters,
with strict checking, IdentitiesOnly and IdentityAgent none. A user's home SSH
file alone does not configure the root daemon. Opaque aliases and arbitrary
sshOpts strings were rejected because they hide routing and weaken validation.
Treat endpoints and remote-program paths as typed data: reject directive/field
injection and encode URI query data once.

The VM route can dial a localhost port through a macOS jump from the Linux
controller and dial directly from macOS. This projects access; it does not create
or start the VM. Keep VM lifecycle owned by its existing configuration.

## Decision 4: Explicit named controller profiles

Use `mandala.build.controllers.<id>`, with explicit selfMember, builder member
allowlists, optional selected lanes and route data. Select profiles explicitly;
never infer from OS, runtime hostname, reachability or evaluator platform.
Exclude all lanes of selfMember by inventory identity, not by endpoint or OS.
Do not exclude a separately enrolled VM hosted there.

Reject unknown member/lane/controller references and incomplete route/key/trust
data. Do not union every profile or fall back to another profile's keys.
Allowlists encode reviewed reachability/authority, not probes during evaluation.
Nix reports unreachable-builder failures and handles scheduling/fallback; do not
re-evaluate once per target or silently reroute to deployment hosts.

Keep the existing deployment.controllerSettings darwin/linux identity map and
precedence untouched. It addresses a different concern. Initial native deploy
continues using the daemon's installed configuration without a new runtime
controller flag.

### Output contract

- `buildMachinesByController.<id>`: a list of ordinary nix.buildMachines-shaped
  attrsets, one per selected member/lane, not a host-keyed attrset.
- `projections.builders.byController.<id>` in the aggregate:
  `{ buildMachines; ssh; knownHosts; }`, with the same machine list and structured
  target/jump routing and public trust data.
- Machine entries carry generated alias, protocol, login, absolute-string key
  path, systems, jobs, speed, supported/mandatory features and encoded public pin
  where applicable. Remote-program query encoding is part of the supported
  connection representation, not a new machine option.
- Aggregate values are only JSON-compatible data, never derivations, modules,
  Nix paths or key contents. Rendered machines-file derivations remain outside.
- With no builders/controllers declared, existing aggregate and deploy/inventory
  bytes remain unchanged, including omission of default-null builder fields and
  the new projection namespace. The direct builder map is empty.
- Inspection describes authored intent, not the daemon's activated state.

Later NixOS and nix-darwin adapters select one profile, set
nix.distributedBuilds=true, builders-use-substitutes=true and nix.buildMachines,
and install system SSH/knownHosts from the same model. Consumers supply imports,
profile selection and data, not projection logic. Use locked module renderers or
one tested engine renderer with golden fixtures rather than independent consumer
string assembly. Each machines-file line represents one lane using Nix's
standard fields, including optional encoded public host key.

If another module contributes the same builder, explicitly choose ownership and
prove parity. A narrowly scoped override at that integration boundary may be
needed; do not silently duplicate capacity or globally force every consumer.

## Decision 5: Later raw per-run override

A later `mandala deploy run --builders <spec>` forwards Nix's own syntax as one
argv value without a shell. Omitted means no argument and unchanged defaults;
present replaces rather than appends; an empty string disables remote builders
but not local building. Preserve omission versus empty across CLI, engine and
supervisor requests. Inline entries and @file retain Nix meanings; define file
resolution at launch and supervisor readability. Do not reinterpret @ as a
Mandala group selector.

Apply the override consistently to initial resolution and the single main batch
build. Never propagate it into the effect-free exact-output recovery probe,
which intentionally forces max-jobs 0, empty builders and substitutes off.
Do not apply it to nix copy or activation. Record intent/source without key
contents or credentials. Unsupported or untrusted daemon settings fail normally;
this grants no privilege, credentials, host trust or key enrollment.

A catalog selector was considered but creates another selector grammar and
runtime controller-selection problem. Native TUI/MCP exposure may follow shared
run options; neither is required for the first projection slice.

## Relationship to binary-cache publication

Choosing where derivations build and publishing the union of built closures
before deployment fan-out are independent, composable phases.
builders-use-substitutes lets builders fetch dependencies; it does not publish
outputs. Standard remote builds still return outputs to the controller.
Controller-side publication alone does not eliminate that hop. Direct
builder-to-cache publication needs a separately shaped design, including
authentication and failure policy. This proposal changes none of those policies,
does not enable deploy-rs remoteBuild, and keeps one batch build graph rather
than per-target builds.

## Delivery slices and acceptance

1. **Schema and pure projection:** builder facet, named controllers, validation,
   lane normalization, self exclusion and JSON-only outputs. Verify with only
   fake-fleet fixtures and byte-identical absent-configuration baselines. This
   is the only implementation scope specified by the current OpenSpec tasks.
2. **Adapters:** shared machine/SSH/knownHosts model into NixOS/nix-darwin,
   golden tests for two lanes, direct/jump routes, remote-program encoding,
   ports, pins/CAs and same-OS controllers. Qualify actual Nix slot/feature
   behavior before making scheduling claims.
3. **Consumer migration:** separate repository and authorization. Capture current
   machine lists and root-effective SSH/trust, inspect existing membership,
   enroll only missing facts-only builders, prove baseline parity, then approve
   any capability/lane reconciliation or allowlist expansion explicitly.
   Preserve VM lifecycle and resolve duplicate module entries deliberately.
   Bounded synthetic daemon-context route/build tests need separate approval.
4. **Run override:** omission/empty/raw/@file request and argv tests, supervisor
   forwarding, unchanged single-build reuse and recovery isolation, normal
   failure propagation, no copy/activation changes.

Repository checks and pure snapshots do not establish live route readiness,
activated daemon configuration, global resource enforcement or authorization to
expand trusted build authority. Public fixtures use acme, globex and
operator-alice only; live baselines and private consumer paths stay outside this
repository.
