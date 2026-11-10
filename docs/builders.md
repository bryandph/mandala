# Builder capacity projections

Build capacity is an optional member fact, not a closure-build instruction or a
deploy identity. This layer is pure inspection: it does not install daemon SSH
configuration, enable distributed builds, start VMs, or prove connectivity.

## Member capacity

Both direct member-schema imports and `lib.evalMember` accept:

```nix
host.builder = {
  systems = [ "x86_64-linux" ];
  supportedFeatures = [ "kvm" "big-parallel" ];
  mandatoryFeatures = [];
  maxJobs = 4;
  speedFactor = 1;
  sshUser = "builder";
  hostname = "builder.acme.example"; # otherwise member.fqdn
  port = 22;
  protocol = "ssh-ng";
  trust = {
    kind = "pin";
    publicKey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
  };
  lanes = {
    wide = {};
    serial = {
      maxJobs = 1;
      supportedFeatures = [];
      mandatoryFeatures = [];
    };
  };
};
```

`builder` defaults to null (no offered capacity). Systems must be explicit and
nonempty. Jobs and speed default to 1 and must be positive integers. Feature
lists default empty; mandatory features must be supported. Lists are sorted and
unique in effective scheduling data. No architecture implies emulation support.

An empty lane map creates one `default` lane; named lanes replace it. Null lane
fields inherit; authored lists replace, including empty feature lists. All
resolved lanes are validated. Lanes sharing an endpoint remain separate machine
entries. Their limits are advertisements, not shared resource reservations.

Facts-only builders use existing `extraMembers` with `lib.evalMember`, not a
second registry. Disable management surfaces (their defaults), and leave
`host.build` null when no closure is managed. Source collisions remain errors.
Enrollment still affects all-member inventory/groups/DNS/docs projections.

## Explicit controller profiles

Import `flakeModules.fleet` (or a tool module importing it) and author:

```nix
mandala.build.controllers.operator-alice = {
  selfMember = "controller";
  allowedBuilders = [ "builder" ];
  lanes.builder = [ "serial" ]; # omitted selects all; [] selects none
  routes.builder = {
    identityFile = "/keys/operator-alice";
    # hostname/sshUser/port/protocol/remoteProgram/trust override defaults
  };
};
```

Profiles are selected explicitly, never by OS, hostname, HOME, or reachability.
`selfMember` must exist even if it offers no capacity. `allowedBuilders` is
required and duplicate-free; an explicit empty list selects nothing. Every
reference is checked, even when self-exclusion removes its output. A separately
enrolled VM is not its controller's identity. Profile and lane labels use
letters, digits, underscores and hyphens, starting with a letter or digit.

Each selected builder needs an explicit route and an absolute **string**
`identityFile`. Keys are never read, imported into the store, or checked for
existence. Login does not inherit deploy root/sudo identity. Routes override
non-null connection fields individually; a trust record replaces inherited
trust completely. A remote program is also an absolute string. Whitespace,
control characters, directive/query delimiters, relative paths, Nix paths, HOME
expansion and arbitrary SSH options are rejected by the typed schema.

Routes can set `jumpMember` to another route in the same profile. All authored
route references and cycles are checked, including unused routes. Jump-only
members need not offer capacity, but their routes need explicit login, endpoint,
key and trust. Every authored route must resolve to complete connection data.
Only selected targets and their transitive jumps are emitted.

Trust is either `{ kind = "pin"; publicKey = "ssh-ed25519 ..."; }` or
`{ kind = "ca"; publicKey = "ssh-ed25519 ..."; principal = "builder.acme.example"; }`.
Keys are canonical public-key text (algorithm and base64 blob, without comments).
A pin cannot declare a principal; a CA requires one. No keyscan, interactive
acceptance, agent identity, or strict-checking bypass exists in this model.

## Output contract

- `buildMachinesByController.<id>` is an ordered list of ordinary
  `nix.buildMachines` records. Order is member then lane, lexicographically.
- Aggregate `mandala.projections.builders.byController.<id>` contains
  `{ buildMachines; ssh; knownHosts; }`; the machine list is exactly the direct
  output, not a separately computed view.
- Non-flake-parts consumers call
  `lib.projections.builders { members = validatedMembers; controllers = profiles; }`.
  The function validates controller types and references and returns the same
  per-profile structured model.

Machines carry `hostName`, `protocol`, `sshUser`, `sshKey`, `systems`, `maxJobs`,
`speedFactor`, `supportedFeatures`, `mandatoryFeatures`, and `publicHostKey`.
`hostName` contains the generated SSH alias and, only when requested, a
once-URI-encoded `?remote-program=` query. There is no extra machine option for
remote programs. A pin's `publicHostKey` base64-encodes the **entire** public-key
text; a CA uses null. Public trust is not a login credential.

`ssh` is keyed by bounded, deterministic aliases generated from JSON tuples of
controller/member/lane identity (separate target and jump namespaces). Records
retain member/lane identity, typed endpoint, user, port, identity path,
remoteProgram, trust, jumpMember/jumpAlias and hostKeyAlias, plus
`strictHostKeyChecking = true`, `identitiesOnly = true`, `identityAgent = "none"`.
Pins use a member-specific host-key alias independent of localhost or lane.
For CA routes, hostKeyAlias is the explicit principal so OpenSSH verifies the
certificate against the authored name rather than the generated dial alias.

`knownHosts` is keyed by the same SSH alias and carries member, hostname, port,
hostKeyAlias, kind, publicKey, principal, matchName, hostNames and certAuthority.
Nondefault ports use bracketed `[hostKeyAlias]:port` matching. The model retains
principal and port explicitly for later system-wide adapters. It is not rendered
SSH text or a machines file, nor a claim of activated daemon configuration.

With no declarations, the direct map is empty, the aggregate builders namespace
is absent, and null facets are omitted from serialized legacy members. Existing
aggregate/deploy/inventory JSON bytes remain unchanged. Facet-only fleets expose
capacity data but no controller namespace. An empty profile emits an empty
machine list and empty SSH/trust maps. Deployment settings and the independent
controller-platform identity precedence are untouched.

## Tests and later work

`builder-schema`, `builder-controllers`, `builder-projection`, and
`builder-compat` are flake checks alongside `fake-fleet`. Synthetic fixtures cover
acme/globex-style roles, two Linux controllers (including WSL), a macOS
controller, a localhost-port Linux VM, and a workstation. Golden JSON in
`tests/fixtures/builder-baseline/` was captured before schema defaults were added,
using `tests/fixtures/builder-legacy.nix`. Additional `managed-*` goldens include
active inventory and direct deploy-rs nodes (only the toolchain is mocked),
using `tests/fixtures/builder-managed-legacy.nix` against the immutable pre-change
source tree. Tests compare exact `builtins.toJSON` bytes rather than reparsing
semantic values.

System adapters, installed trust/SSH, daemon-context qualification, consumer
migration, lane slot-accounting qualification, and deploy `--builders` overrides
are separate slices. Native deployment still uses installed daemon defaults.
