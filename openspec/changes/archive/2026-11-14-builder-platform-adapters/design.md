## Context

The normalized contract is in lib/builders.nix and the first projection change.
The engine's sole input remains nixpkgs. No fleet values enter these modules.

## Decisions

1. Both module exports import one common module. mandala.builders.enable opts in;
   controller is an explicit required string and byController is a passed-in
   projection map, never a host evaluation or runtime lookup. Unknown selection
   fails. Alternative OS/hostname inference is rejected as ambiguous and impure.
2. nix.buildMachines receives the selected list at ordinary module priority.
   exclusiveBuildMachines defaults false; true uses a narrowly scoped mkForce
   only on nix.buildMachines. It deliberately discards other contributions.
   Silent deduplication or forcing by default would hide capacity ownership.
3. lib.renderBuilders consumes one normalized profile and returns sshConfig,
   knownHosts (native module attrsets), knownHostsText, and machinesFile strings.
   The SSH and trust renderer is shared, not copied per platform. It validates
   every rendered token and fixed strict policy before interpolation, even though
   the projection is already validated. Arbitrary directives are not accepted.
4. Modules set distributedBuilds and builders-use-substitutes normally (not
   mkDefault or mkForce), and prepend alias blocks with mkBefore to system-wide
   programs.ssh.extraConfig so earlier wildcard defaults do not shadow them.
   SSH Host * closes the block to avoid leaking following configuration.
   programs.ssh.knownHosts receives namespaced entries normally.
5. rendered is a read-only module option exposing the renderer output for
   inspection; machinesFile is a pure string, never a derivation or IFD.
   Alternatives omitting inspection or creating a store file obscure parity.
6. NixOS is evaluated against the real pinned modules; nix-darwin is evaluated
   with minimal typed stub declarations, not a new flake input. Both use the
   exact same module and renderer; this is not Darwin activation qualification.

## Risks and boundaries

OpenSSH is first-value-wins: module mkBefore is ordered ahead of ordinary
extraConfig contributions, but earlier explicit mkBefore blocks or user SSH
configuration can still interfere. Adoption must inspect daemon-effective SSH.
Exclusive ownership resolves competing buildMachines contributions only when
explicitly selected. Features and shared-endpoint slot accounting need
qualification on the deployed Nix version; tests prove authored intent only.
No changes to slice-1 projections, global trust bypass, credentials or deployment.

## Verification

Exact goldens cover both platform adapters, pins and CA, direct and jump routes,
nondefault ports, query encoding, two lanes per endpoint, full publicHostKey
encoding, and empty profiles. Reject unsafe renderer input. Check normal merges,
exclusive ownership, disabled state and unknown controller errors. Run all
builder-* checks, fake-fleet, formatting, and the complete CI command list.
