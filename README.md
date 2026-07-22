# mandala

Fleet contract engine for Nix flakes: describe a heterogeneous fleet once —
in your configurations — and project that single source outward to the
tools that already manage fleets well: deploy-rs, ansible, sops, DNS/DHCP,
overlay networks, terraform.

## The arrow points outward

Most fleet frameworks generate machine configuration downward from an
inventory, and bring their own orchestration runtime with them. mandala
inverts the arrow: **your configurations are the inventory**. A NixOS host
authors its own facts (`config.host.*` per the member schema); a non-NixOS
member (router, switch, AP, BMC, Windows box) is plain data validated
against the same schema. The engine validates what the fleet declares and
computes the views other systems consume — it never generates a host
configuration, and it never replaces the tools that deploy, configure, or
distribute secrets.

That buys two things frameworks can't offer:

- **A tiny, exit-friendly buy-in surface.** Adopt mandala and your
  `nixosConfigurations` are still yours; rip it out and they still stand.
- **Native drift detection.** Because the contract is independent of the
  machines, there is always something to diff live state against.

```
mandala            engine (this repo): schema + lib, flake inputs = nixpkgs only
└── <operator>     data flake: values filling the schema (private or public)
    └── consumers  infra flakes pin the data flake and read projections
```

The engine deliberately contains no fleet — no VLAN, no key, no address.
An operator publishes a *data flake* filling the schema; consumers pin that
one input and get validated data, the schema modules, and this library
together.

## Outputs

- `lib.schemas.{operator,topology,member,mesh,pki}` — module paths
  declaring the contract.
- `lib.eval{Operator,Topology,Member,Mesh,Pki}` — validate data against a
  schema and return it with derived fields; invalid data fails the
  consumer's eval, not a later deploy.
- `lib.groupsFor` / `lib.ansibleGroupsFor` / `lib.sanitizeGroupName` — the
  one group taxonomy (and its one fan-out spelling) behind deploy-rs
  groups, `ansible -l`, and sops recipient sets, so the authorities cannot
  drift.
- `lib.facter` — nixos-facter report predicates; reports corroborate
  authored facts, they never set them.
- `checks.<system>.fake-fleet` — the engine evaluated against the bundled
  fake fleet in `examples/fake-fleet/` (operator-value-free).

## Status / roadmap (pre-1.0)

The projection layer is being lifted into this repo: pure projection
functions (`lib.projections` — ansible inventory, sops config, deploy-rs
nodes, eval-once batch builds), flake-parts shim modules, a secret-grade
secrets schema, a native CLI/TUI deploy engine, and a minimal
`mandala.fleet` ansible collection for the read-only state survey, plus
`nix flake init` templates and a showcase fleet. Until 1.0, schemas and lib
signatures may change without notice; aggregate outputs carry a
`schemaVersion` so porcelain can keep up.

The native deploy porcelain folds Nix's structured build events into one
dependency forest shared by the TUI, headless CLI output, and MCP status.
The build tab renders that state directly; it does not launch `nom` or host a
terminal emulator, so the same counts, current activity, and failed derivation
attribution remain available in non-interactive and CI runs.

## Design

Configs author the inventory; projections flow outward. The engine never
generates host configurations from inventory — it validates facts the
configurations (and facts-only members) declare, and computes the views
other systems consume. Toolchains (deploy-rs, nixpkgs package sets) are
injected as function arguments, never engine inputs: the engine's only
flake input is nixpkgs, and lib-only consumers evaluate nothing else.
