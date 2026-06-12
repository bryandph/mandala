# The terranix pattern: terraform consumes the fleet contract DIRECTLY
# as module arguments — no intermediate .tf.json data files, no second
# source of truth. `fleet` is the aggregate (flake.mandala), `mesh` and
# `topology` are validated engine data; all three arrive via extraArgs.
#
# The provider here is fictional (nothing applies this); the point is
# the shape: records derive from members' dns-role addresses and the
# mesh table, so adding a member updates the zone on the next plan.
{
  fleet,
  mesh,
  lib,
  ...
}: let
  dnsAddressOf = member:
    lib.findFirst (n: lib.elem "dns" n.roles) null member.networks;

  forward =
    lib.filterAttrs (_: v: v != null)
    (lib.mapAttrs (_: dnsAddressOf) fleet.members);
in {
  resource.dns_a_record_set =
    lib.mapAttrs (name: net: {
      zone = "fleet.example.";
      name = name;
      addresses = [net.address];
      ttl = 300;
    })
    forward
    // lib.mapAttrs' (key: m:
      lib.nameValuePair "mesh-${key}" {
        zone = "mesh.";
        name = lib.head (lib.splitString "." (m.dnsName or key));
        addresses = [m.ip];
        ttl = 300;
      })
    (lib.filterAttrs (_: m: m.dnsName != null) mesh.members);
}
