# Pattern: terraform via terranix, contract-direct

Terraform consumes the fleet contract DIRECTLY inside terranix modules —
no exported `.tf.json` data files, no second source of truth that can
drift. The fleet data arrives as module arguments:

```nix
perSystem = {system, ...}: {
  packages.network-tf = inputs.terranix.lib.terranixConfiguration {
    inherit system;
    modules = [./terranix/dns.nix];
    extraArgs = {
      fleet = config.flake.mandala;   # the aggregate: members + groups
      inherit mesh topology;          # validated engine data (evalMesh/evalTopology)
    };
  };
};
```

and the module derives resources from it (working example:
`examples/showcase/terranix/dns.nix` — DNS records from members'
dns-role addresses plus the mesh table):

```nix
{fleet, mesh, lib, ...}: {
  resource.dns_a_record_set = lib.mapAttrs (name: member: {
    zone = "fleet.example.";
    inherit name;
    addresses = [(dnsAddressOf member).address];
  }) fleet.members;
}
```

Why this shape:

- **Adding a member updates the plan.** The next `tofu plan` sees the
  new record because the data flows from the same eval that builds the
  host — drift detection falls out for free (plan diff = drift).
- **Addresses with roles, not flags.** A member's DNS name can resolve
  to a mesh address while it default-routes elsewhere; filter on
  `roles` (`dns`, `reach`, `gateway`, `management`), never on position.
- **Resource keys are state addresses.** Mesh table keys become
  terraform resource names — renaming one is a destroy/create. Choose
  stable keys.
- **Reservations carry through.** `assignment = "reservation"` entries
  (with their `mac`) drive DHCP-reservation resources the same way.
