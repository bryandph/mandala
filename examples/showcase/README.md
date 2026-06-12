# Showcase: a complete fictional fleet

Three members — `web` (x86_64 NixOS), `cache` (aarch64 NixOS, DHCP
reservation), `router` (facts-only OPNsense gateway) — plus an operator,
a topology, a mesh table, and secret declarations. Every projection the
engine ships is exercised here; nothing names a real fleet.

```sh
nix flake check                          # asserts every projection's shape
nix eval --json .#mandala | jq .groups   # the aggregate, one eval
nix build .#network-tf && cat result     # the terranix render
```

## Asserted vs illustrated

`checks.showcase` ASSERTS (pure evaluation — CI-enforced, cannot rot):

- the aggregate (`flake.mandala`): schemaVersion, member set, taxonomy
  groups (including the facts-only member), operator carriage
- the ansible inventory: membership filter, hook hostvars, the NixOS
  python pin, the `deploy_rs` guard group
- the sops config: recipient keys, pgp-only admin rule
- deploy nodes + batch: node attrs and group keys (evaluated, not built)
- the mesh table validates and derives DNS names
- `packages.network-tf`: the terranix DNS render from members + mesh

Only ILLUSTRATED (a demo fleet has nothing real to push to):

- actually running the fan-out deploy (`mandala.fleet.deploy`) or
  activating with deploy-rs
- applying the terranix output with terraform/tofu
- ansible runs against the inventory (the members don't exist)
- non-NixOS member management end to end (the router is data only)
