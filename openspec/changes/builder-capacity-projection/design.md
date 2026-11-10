## Context

Member facts live in `schema/member.nix`; `schema/operator.nix` describes operator
identity, not infrastructure capacity. `lib/default.nix` owns pure projections,
and `flake-modules/fleet.nix` assembles members and the JSON aggregate.
`docs/convention.md` describes the existing member enrollment and deployment
controller-platform contract. Those files remain the authority for existing
behavior; this change does not duplicate their current values.

The whole approved direction, including deferred slices, is recorded in
[docs/proposals/builder-capacity.md](../../../docs/proposals/builder-capacity.md).
Only the new `builder-capacity` delta is normative for this implementation slice.

## Goals / Non-goals

Goals: one capacity source per member; explicit controller selection; pure,
deterministic output; strict identity/trust validation; unchanged legacy output
when absent; verification entirely with fictional fleets.

Non-goals: adapters or rendered machines files, consumer migration, live
qualification, runtime flags, binary-cache publishing, global capacity scheduling,
or any change to existing deploy identity precedence. Future work is described
below, not scheduled in tasks.md.

## Decisions

### D1: Capacity is an optional member facet

Use `host.builder = null | { ... }`, default null. Keep `host.build` as closure
build instructions and keep operator identity independent. Builders without
managed closures use the existing `extraMembers` / `evalMember` path with no
management surface enabled. Do not invent a network attachment or permit
collisions with configuration-derived members.

The facet contains explicit nonempty `systems`, canonical feature sets,
positive-integer `maxJobs` and `speedFactor` (both default 1), named `lanes`,
and connection defaults. Features default empty; mandatory features must be a
subset of supported features. Architecture and closure build system imply no
execution/emulation capability. Connection defaults cover hostname, builder
login, port (22), protocol (initially only ssh-ng), optional remote-program path,
and public trust. A missing hostname may resolve to the member's primary FQDN;
if neither exists, a selected route must supply it. Builder login is explicit,
never inherited from deploy root/sudo identity.

Alternative: a separate operator builders registry. Rejected because member
builders would duplicate identity and need cross-registry precedence. Enrollment
has observable all-member inventory/group/DNS/docs effects; adoption must review
those, even though a facts-only member gains no push target.

### D2: Lanes are replacement views, not capacity reservations

An absent or empty lane map yields one `default` lane. A nonempty map replaces it.
Each named lane has nullable overrides for systems, supportedFeatures,
mandatoryFeatures, maxJobs, and speedFactor; null inherits the facet value.
An explicit list replaces the inherited list, including an empty feature list.
Validate the fully resolved lane, not only its authored fields. Connection
defaults are shared by lanes; lane distinctions do not change member identity.

Sort members and lanes lexicographically; canonicalize set-valued systems and
features with sorted unique values. Retain different lanes on the same endpoint.
Do not sum them into a promised physical budget or deduplicate by hostname.

Alternative: append inherited features and add an implicit default beside named
lanes. Rejected because it silently increases advertised capacity and prevents a
lane from removing a capability. A serial high-memory lane does not reserve memory
against wide-lane work or another controller. Deployed Nix slot accounting and
feature matching remain an explicit later qualification gate.

### D3: Named profiles separate policy from controller OS

`mandala.build.controllers` defaults to an empty map. Each profile has:

| Field | Contract |
| --- | --- |
| `selfMember` | Required existing member ID; identifies the controller even if it offers no builder facet. |
| `allowedBuilders` | Explicit duplicate-free member-ID allowlist; each entry must offer a non-null facet. Empty means no builders. |
| `lanes` | Optional member-keyed lists of lane names; keys must be allowed builders. Omission selects all effective lanes; an explicit empty list selects none. |
| `routes` | Member-keyed structured route records, including any jump-only member needed by selected routes. |

Profile and lane names must be nonempty safe labels suitable for stable aliases,
with no whitespace, control characters, directive delimiters, or alias collisions.
No OS/hostname/HOME/environment inference selects a profile. Consumers explicitly
name it; an unknown requested profile is an error. A profile returns no lane of
`selfMember`, even if explicitly allowlisted, but retains a separate VM member
hosted there. Validate authored references even if self-exclusion removes output.

Alternative: extend `deployment.controllerSettings` with machine names or select
by runtime hostname. Rejected: OS identity and per-machine policy are distinct,
and runtime discovery makes evaluation impure. That strict darwin/linux map and
its precedence remain untouched. Do not union controller allowlists or use a
different profile as a key fallback.

### D4: Routes are typed and trust is explicit

Routes override member connection defaults field by field; authored scalar
values replace, and a trust declaration replaces the entire inherited declaration.
A route carries `hostname`, `sshUser`, `port`, `protocol`, `identityFile`,
optional `remoteProgram`, optional `jumpMember`, and optional `trust`. Selected
builders require a route providing an identity; jump-only routes may refer to
facts-only members without a builder facet and must provide their own effective
connection, key, and trust data. All route keys name existing members; every
jumpMember names a route in the same profile. Resolve the whole authored route
graph and reject cycles, including self-loops, and missing references.

`identityFile` is an absolute string, never a Nix path, key contents, readFile,
or an environment/HOME expansion. Do not read it or require its existence during
pure evaluation. Remote-program paths are absolute strings too. Public trust is
a tagged choice: an explicit SSH public-key text pin, or a public host CA with an
explicit matching host name/principal. No empty trust declaration, interactive
acceptance, runtime keyscan, or strict-checking bypass is allowed. Each jump and
final endpoint has trust; ordinary login identity is not host trust.

Project stable, collision-free SSH aliases and host-key aliases from controller,
member, and lane identities, with deterministic escaping/length handling. Keep
a localhost VM's host identity separate from its controller/jump host and retain
nondefault port information. Produce a single canonical structured SSH model
requiring strict checking, IdentitiesOnly, and IdentityAgent none; later adapters
install it system-wide for the daemon, not just in a user's SSH file.

Alternative: opaque aliases or arbitrary sshOpts text. Rejected because hidden
routing and permissive trust cannot be validated. Endpoint fields are data, not
raw URI/query/SSH directives: reject newline/control and delimiter injection and
encode remote-program query data once. Public-key text, Nix publicHostKey
(base64 of the full public-key text), and knownHosts records are distinct
representations; fixtures must assert the conversion and port/name mapping.

### D5: Two outward outputs, one canonical projection

`buildMachinesByController.<id>` is a list of ordinary nix.buildMachines-shaped
attrsets, preserving one entry per selected member/lane. Its aggregate counterpart
is `projections.builders.byController.<id> = { buildMachines; ssh; knownHosts; }`.
`buildMachines` is identical to the direct list. `ssh` carries structured target
and jump routes keyed by generated alias, including endpoint, user, port, key path,
jump alias, host-key alias and strict identity/trust policy. `knownHosts` carries
public pin/CA entries and explicit host/port/principal matching information.
Stable member/lane identity remains recoverable from aliases and structured data.

Machine fields are hostName (generated alias), protocol, sshUser, sshKey (absolute
string), systems, maxJobs, speedFactor, supportedFeatures, mandatoryFeatures,
and publicHostKey (encoded pin where applicable, null for the explicit CA model).
An optional remoteProgram is encoded into the machine connection representation,
not emitted as an unrecognized nix.buildMachines field. Structured routing retains
it as a typed field. Rendering a Nix machines file or SSH text is later work.

Both outputs derive from one normalized model. The aggregate contains only
JSON-compatible attrs/lists/strings/numbers/booleans/null, no derivations, module
values or Nix paths, and no private material. Evaluation does not inspect the
network, filesystem keys, activated daemon config, or runtime platform.

Alternative: key the machines output by endpoint. Rejected because multiple lanes
may intentionally share it. A machines-file derivation in the aggregate would
violate the pure inspection contract.

### D6: Preserve absence and deploy behavior

With no builder facets or controller profiles authored, preserve the existing
aggregate byte-for-byte: omit default-null builder fields from serialized legacy
members and omit the new builders projection namespace. The separate direct
builder map is empty. Captured legacy aggregate, deploy, and inventory JSON are
golden byte-comparison gates, not merely semantic equality checks.

Facet-only declarations may be inspected as member data without selecting a
controller; no controller means no projected builder namespace. A declared empty
profile has explicit empty machine/SSH/trust outputs rather than selecting the
whole fleet. New configuration never rewrites existing deploy projections.
Native builds still use the installed controller daemon defaults in this slice;
there is no claim that the aggregate describes the currently activated daemon.

## Verification

Use fictional acme/globex fleets and operator-alice only. Include a Linux
controller under WSL, a macOS controller, two controllers sharing an OS, a
localhost-port Linux builder VM, and an x86_64 workstation builder. Keep fixtures
pure; public test keys are synthetic, never live credentials.

Planned targeted checks are `builder-schema`, `builder-controllers`,
`builder-projection`, and `builder-compat`, built individually as
`nix build .#checks.<system>.<name>`. These are implementation deliverables, not
checks this documentation-only change claims to add. Fully force invalid nested
data and JSON serialization, test both direct member modules and evalMember,
permute map/list authoring order, compare exact baselines, and retain existing
checks. Run `nix fmt -- --check .` and `nix flake check` as final gates.

## Future work (not implementation tasks in this change)

1. **Adapters:** NixOS/nix-darwin consume an explicitly selected profile, set
   distributedBuilds and builders-use-substitutes, install the machine list and
   system SSH/knownHosts, and share one tested renderer. Golden tests cover pins,
   CAs, jump/direct paths, ports and remote-program encoding. Qualify lane feature
   and shared-endpoint slot accounting on the deployed Nix version.
2. **Consumer migration:** separately authorized repository work captures existing
   machine lists and daemon-effective SSH/trust first; enrolls only missing
   members; proves parity before changing systems, lanes or allowlists; resolves
   module-contributed duplicate builders explicitly. Preserve VM lifecycle.
   Bounded synthetic daemon-context builds require separate operator approval.
3. **Raw run override:** later add optional Nix-compatible `--builders <spec>`.
   Omission preserves defaults, presence replaces, empty disables remote building
   only, and @file keeps Nix semantics. Carry omission/empty intent through run
   requests and supervisor argv without a shell; define @file launch resolution.
   Apply to initial resolution and the single batch build, never nix copy,
   activation, or the effect-free recovery probe that forces builders empty.
   Preserve normal errors and daemon privilege boundaries.

Binary-cache publication is independent: builders-use-substitutes fetches build
dependencies, not publication of results. Remote outputs still return to the
controller; direct builder-to-cache publication needs its own approved design.

## Risks and approval boundaries

Route normalization must not accidentally turn authored data into directives.
Alias collision and host trust conversion tests precede adapters. Per-controller
limits are advertisements, not global reservations. Output checks do not prove
daemon reachability or scheduling. This document records the selected design;
artifact completeness does not authorize implementation, migration, key changes,
trust expansion, or deployment.
