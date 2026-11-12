# Builder capacity projections

Build capacity is an optional member fact, not a closure-build instruction or a
deploy identity. The projection layer is pure inspection. Optional system adapters below install
its intended daemon configuration; neither layer manages builder lifecycle or proves connectivity.
VM members are out of scope pending a separate host-type design.

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
enrolled builder is not its controller's identity. Profile and lane labels use
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
A pin cannot declare a principal. A CA's optional `principal` defaults to the
resolved dialled hostname: route hostname, then member builder hostname, then
member FQDN. Explicit principals win. This is the endpoint name alone, not the
port, jump hostname or generated SSH alias. The certificate must cover that name;
missing or invalid endpoint data fails evaluation, never weakening trust.
No keyscan, interactive
acceptance, agent identity, or strict-checking bypass exists in this model.
See the [CA endpoint-default change](../openspec/changes/builder-ca-endpoint-default/proposal.md)
for the additive update to the original explicit-principal contract.

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
Pins use a member-specific host-key alias independent of endpoint spelling or lane.
For CA routes, hostKeyAlias is the resolved principal so OpenSSH verifies the
certificate against the explicit or defaulted name rather than the generated dial alias.

`knownHosts` is keyed by the same SSH alias and carries member, hostname, port,
hostKeyAlias, kind, publicKey, principal, matchName, hostNames and certAuthority.
Nondefault ports use bracketed `[hostKeyAlias]:port` matching. The model retains
principal and port explicitly for the system-wide adapters. The projection itself
is not rendered SSH text or a machines file, nor activated daemon configuration.

With no declarations, the direct map is empty, the aggregate builders namespace
is absent, and null facets are omitted from serialized legacy members. Existing
aggregate/deploy/inventory JSON bytes remain unchanged. Facet-only fleets expose
capacity data but no controller namespace. An empty profile emits an empty
machine list and empty SSH/trust maps. Deployment settings and the independent
controller-platform identity precedence are untouched.

## Opt-in NixOS and nix-darwin adapters

Import `mandala.nixosModules.builders` on NixOS or
`mandala.darwinModules.builders` on nix-darwin. Both exports use the same module
and `mandala.lib.renderBuilders` pure renderer. Pass the **already evaluated**
projection from the data flake; do not re-evaluate fleet members in each host:

```nix
{
  imports = [ mandala.nixosModules.builders ]; # or darwinModules.builders
  mandala.builders = {
    enable = true;
    controller = "operator-alice";
    byController = fleet.mandala.projections.builders.byController;
  };
}
```

`mandala.builders` options:

| Option | Default / behavior |
| --- | --- |
| `enable` | false; disabled imports contribute no daemon/SSH/trust settings |
| `controller` | Required when enabled; explicit profile ID, unknown ID fails |
| `byController` | Empty map; pass the existing aggregate projection map |
| `exclusiveBuildMachines` | false; explicitly opt into discarding other machine contributions |
| `rendered` | Read-only selected renderer output, available when enabled |

Enabled adapters set `nix.distributedBuilds = true`,
`nix.settings.builders-use-substitutes = true`, and the selected
`nix.buildMachines` list at **ordinary module priority** (100), not mkDefault.
Other modules' machine lists merge normally; there is no implicit deduplication.
Review entries contributed by other modules before adoption: two declarations
of the same capacity can double-advertise it. If this adapter intentionally owns
the whole list, set `exclusiveBuildMachines = true`. This applies `mkForce`
(priority 50) to **only** `nix.buildMachines`, not to SSH, trust or other Nix
settings. This is an explicit integration-boundary decision, not a safe default.
No deployment identity precedence or slice-1 output is changed.

The system-wide `programs.ssh.extraConfig` gets deterministic alias blocks with
hostname, login, port, IdentityFile, optional ProxyJump, HostKeyAlias,
IdentitiesOnly yes, IdentityAgent none and StrictHostKeyChecking yes. A final
`Host *` resets the scope for following configuration. Blocks use `mkBefore`
(order 500) ahead of ordinary extraConfig (order 1000), because OpenSSH uses the
first value found for most settings. Earlier explicit mkBefore blocks or root's
own SSH configuration can still interfere; inspect daemon-effective options
before adoption. The invoking user's `~/.ssh/config` is **not read by the root
nix-daemon**; system ssh_config is the adapter's only target. This module does
not write user/home configuration or enroll private keys. The daemon must already
be able to read the authored identity paths noninteractively.

`programs.ssh.knownHosts` receives normally merged `mandala-<alias>` entries
with explicit hostNames, publicKey and certAuthority. Pins and CA principals,
including bracketed nondefault ports, retain their distinct representations.
IdentityFile percent characters are escaped for OpenSSH's percent-token syntax.
Neither rendered text nor inspection reads any private key file.

`mandala.lib.renderBuilders selectedProfile` and
`config.mandala.builders.rendered` expose `{ sshConfig; knownHosts;
knownHostsText; machinesFile; }`. These are pure strings/attrsets, not derivations
or IFD. `machinesFile` uses Nix's standard eight fields for Nix >= 2.4, with `-`
for empty features/null publicHostKey and the projection's already encoded
remote-program query. It matches the native module renderer, including appending
mandatory features to supported features (which can repeat tokens). It is an
inspection of the selected list only, **not** of other modules' merged entries,
nor proof of installed daemon state. The adapter installs the ordinary
`nix.buildMachines` list rather than overriding `/etc/nix/machines` itself.

## Tests and later work

`builder-schema`, `builder-controllers`, `builder-projection`, and
`builder-compat` are flake checks alongside `fake-fleet`. Synthetic fixtures cover
acme/globex-style roles, two Linux controllers (including WSL), a macOS
controller acting as a bastion, a non-VM Linux builder reached on a nondefault
port (through the bastion or directly), and a workstation. Golden JSON in
`tests/fixtures/builder-baseline/` was captured before schema defaults were added,
using `tests/fixtures/builder-legacy.nix`. Additional `managed-*` goldens include
active inventory and direct deploy-rs nodes (only the toolchain is mocked),
using `tests/fixtures/builder-managed-legacy.nix` against the immutable pre-change
source tree. Tests compare exact `builtins.toJSON` bytes rather than reparsing
semantic values.

`builder-renderer` compares exact renderer JSON goldens for jump and direct
routes, pins and CA, nondefault ports, remote-program percent encoding, two
lanes on one endpoint, full-key publicHostKey encoding and an empty profile.
It also rejects injected directives/field delimiters and non-strict policy.
`builder-adapters` compares exact complete adapter goldens on both platforms,
checks ordinary merges and exclusive ownership, disabled/missing selections,
conflicting settings and trailing extraConfig scope. It evaluates NixOS with the
real pinned modules and checks native machines-file text against the renderer.
Darwin uses minimal typed option declarations in
`tests/fixtures/builder-darwin-options.nix`, checked read-only against
[nix-darwin revision ff0e21a414d4547183396da1b44a66f2577b65e9](https://github.com/nix-darwin/nix-darwin/tree/ff0e21a414d4547183396da1b44a66f2577b65e9)
(`modules/nix/default.nix` and `modules/programs/ssh.nix`), not a nix-darwin input.
`builder-ssh-scope` runs OpenSSH's offline `-G` parser against rendered blocks
followed by an ordinary global directive; it does not connect anywhere.

These checks do **not** qualify Darwin activation, daemon-context routing, live
host trust or scheduling. Lane feature matching and shared-endpoint slot
accounting must be qualified on the **deployed Nix version** before making those
claims. Per-lane maxJobs is not a shared-endpoint or cross-controller budget.
Consumer migration and deploy `--builders` overrides remain separate slices.
Native deployment still uses installed daemon defaults.
