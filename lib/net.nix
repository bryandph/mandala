# Address-derivation primitives — realize a `{vlan, id}` authoring into
# concrete IPv4 + ULA addresses from validated topology data, so host
# and member definitions never restate subnet or ULA prefixes.
#
# Conventions (mirrored in the member schema's `id` description):
# - `id` is the integer host offset from the VLAN subnet's network
#   address: id 102 on 172.16.15.0/24 → 172.16.15.102; id 261 on
#   10.5.0.0/16 → 10.5.1.5.
# - The ULA host part is the id spelled as ONE decimal-literal hex group
#   on the VLAN's ULA /64: id 102 → ::102, id 261 → ::261. The id maps
#   to both address families directly; nothing is derived from the
#   rendered v4 string. Ids are capped at 9999 so the literal always
#   fits one group.
#
# Only whole-octet prefix lengths (8/16/24) are supported for v4 math.
{lib}: rec {
  ip4ToInt = ip:
    lib.foldl' (a: o: a * 256 + lib.toInt o) 0 (lib.splitString "." ip);

  intToIp4 = n:
    lib.concatStringsSep "." (map toString [
      (lib.mod (n / 16777216) 256)
      (lib.mod (n / 65536) 256)
      (lib.mod (n / 256) 256)
      (lib.mod n 256)
    ]);

  # v4 host address on a VLAN: network address + id.
  v4 = vlanCfg: id: let
    base = lib.head (lib.splitString "/" vlanCfg.subnet);
    hostBits = 32 - vlanCfg.prefixLength;
    maxHosts = lib.foldl' (a: _: a * 2) 1 (lib.range 1 hostBits);
  in
    assert lib.assertMsg (vlanCfg.subnet or null != null)
    "mandala net.v4: VLAN has no authored subnet";
    assert lib.assertMsg (lib.mod vlanCfg.prefixLength 8 == 0)
    "mandala net.v4: only whole-octet prefix lengths are supported";
    assert lib.assertMsg (id > 0 && id < maxHosts - 1)
    "mandala net.v4: id ${toString id} outside host range of ${vlanCfg.subnet}";
      intToIp4 (ip4ToInt base + id);

  # ULA host address on a VLAN: the id as one decimal-literal hex group.
  ula = vlanCfg: id:
    assert lib.assertMsg (vlanCfg.ula or null != null)
    "mandala net.ula: VLAN has no authored ULA prefix";
    assert lib.assertMsg (id > 0 && id <= 9999)
    "mandala net.ula: id ${toString id} does not fit one decimal-literal group"; "${lib.removeSuffix "::/64" vlanCfg.ula}::${toString id}";

  # ULA for an attachment that authors a v4 address instead of an id:
  # recover the id (offset from the network address), then derive.
  ulaFromV4 = vlanCfg: address:
    ula vlanCfg
    (ip4ToInt address - ip4ToInt (lib.head (lib.splitString "/" vlanCfg.subnet)));

  # Resolve a raw networks list (pre-schema data): entries carrying an
  # `id` gain derived address/ula where not explicitly authored. Entries
  # without `id` pass through untouched. ULA derivation is skipped when
  # the VLAN authors no ULA prefix.
  resolveNetworks = topology: networks:
    map (
      n:
        if (n.id or null) == null
        then n
        else let
          vlanCfg = topology.vlans.${n.vlan};
        in
          n
          // {
            address =
              if (n.address or null) != null
              then n.address
              else v4 vlanCfg n.id;
            ula =
              if (n.ula or null) != null
              then n.ula
              else if (vlanCfg.ula or null) != null
              then ula vlanCfg n.id
              else null;
          }
    )
    networks;

  # Partial application over validated topology — the name-keyed API host
  # and member definitions author against.
  forTopology = topology: let
    vlanOf = name: topology.vlans.${name};
  in {
    address = name: id: v4 (vlanOf name) id;
    ula = name: id: ula (vlanOf name) id;
    ulaFromV4 = name: addr: ulaFromV4 (vlanOf name) addr;
    # A complete network-attachment seed: merge roles/interface/… onto it.
    host = name: id: let
      vlanCfg = vlanOf name;
    in {
      vlan = name;
      inherit id;
      address = v4 vlanCfg id;
      ula =
        if (vlanCfg.ula or null) != null
        then ula vlanCfg id
        else null;
    };
  };
}
