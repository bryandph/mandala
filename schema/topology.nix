# Network topology schema: VLAN/network definitions keyed by name.
# Authored once in the operator-data flake; consumers read
# `topology.vlans.<name>.*`.
#
# The IPv4 prefix length DERIVES from the subnet CIDR — only the CIDR is
# hand-authored. Entries may be id-only (subnet = null) for networks that
# exist on switch trunks but have no routed subnet authored yet.
{
  config,
  lib,
  ...
}: let
  inherit (lib) mkOption types;
  decimal = value: builtins.match "[0-9]+" value != null;
  ipv4 = value: let
    octets = lib.splitString "." value;
  in
    lib.length octets
    == 4
    && lib.all (octet: decimal octet && (let n = lib.toInt octet; in n >= 0 && n <= 255)) octets;
  ipv4Cidr = value: let
    parts = lib.splitString "/" value;
  in
    lib.length parts
    == 2
    && ipv4 (lib.head parts)
    && decimal (lib.last parts)
    && (let prefix = lib.toInt (lib.last parts); in prefix >= 0 && prefix <= 32);
  ipv6 = value: let
    halves = lib.splitString "::" value;
    groups = lib.concatMap (half: lib.filter (group: group != "") (lib.splitString ":" half)) halves;
    compressed = lib.length halves == 2;
  in
    lib.length halves
    <= 2
    && lib.all (group: builtins.match "[0-9A-Fa-f]{1,4}" group != null) groups
    && (
      if compressed
      then lib.length groups < 8
      else lib.length groups == 8
    );
  ipv6Cidr = value: let
    parts = lib.splitString "/" value;
  in
    lib.length parts
    == 2
    && ipv6 (lib.head parts)
    && decimal (lib.last parts)
    && (let prefix = lib.toInt (lib.last parts); in prefix >= 0 && prefix <= 128);
  ip = value: ipv4 value || ipv6 value;

  validate = topology: let
    vlanNames = lib.attrNames topology.vlans;
    badVlanIds = lib.filter (name: let id = topology.vlans.${name}.id; in id != 0 && (id < 1 || id > 4094)) vlanNames;
    badSubnets = lib.filter (name: let value = topology.vlans.${name}.subnet; in value != null && !ipv4Cidr value) vlanNames;
    badGateways = lib.filter (name: let value = topology.vlans.${name}.gateway; in value != null && !ipv4 value) vlanNames;
    badDns = lib.filter (name: !lib.all ip topology.vlans.${name}.dns) vlanNames;
    badUlas = lib.filter (name: let value = topology.vlans.${name}.ula; in value != null && (!ipv6Cidr value || lib.last (lib.splitString "/" value) != "64")) vlanNames;
    badUlaGateways = lib.filter (name: let value = topology.vlans.${name}.ulaGateway; in value != null && !ipv6 value) vlanNames;
  in
    assert lib.assertMsg (badVlanIds == [])
    "topology VLAN ids must be 0 or 1..4094: ${lib.concatStringsSep ", " badVlanIds}";
    assert lib.assertMsg (badSubnets == [])
    "topology subnets must be valid IPv4 CIDRs: ${lib.concatStringsSep ", " badSubnets}";
    assert lib.assertMsg (badGateways == [])
    "topology gateways must be valid IPv4 addresses: ${lib.concatStringsSep ", " badGateways}";
    assert lib.assertMsg (badDns == [])
    "topology DNS servers must be valid IP addresses: ${lib.concatStringsSep ", " badDns}";
    assert lib.assertMsg (badUlas == [])
    "topology ULA prefixes must be valid IPv6 /64 CIDRs: ${lib.concatStringsSep ", " badUlas}";
    assert lib.assertMsg (badUlaGateways == [])
    "topology ULA gateways must be valid IPv6 addresses: ${lib.concatStringsSep ", " badUlaGateways}"; topology;

  vlanType = types.submodule ({config, ...}: {
    options = {
      id = mkOption {
        type = types.int;
        description = "VLAN ID (use 0 for non-VLAN networks like mesh overlays).";
      };
      subnet = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "IPv4 CIDR; null for id-only networks.";
        example = "172.16.15.0/24";
      };
      prefixLength = mkOption {
        type = types.nullOr types.int;
        internal = true;
        default =
          if config.subnet == null
          then null
          else if ipv4Cidr config.subnet
          then lib.toInt (lib.last (lib.splitString "/" config.subnet))
          else null;
        defaultText = "prefix length of subnet";
        description = "IPv4 prefix length for host addresses (derived from subnet).";
      };
      gateway = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "IPv4 gateway address.";
      };
      dns = mkOption {
        type = types.listOf types.str;
        default = [];
        description = "DNS servers for this network.";
      };
      domain = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Search domain.";
        example = "servers.example";
      };
      description = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Human-readable label.";
      };
      ula = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "ULA /64 prefix (stable internal IPv6).";
        example = "fd00:dead:beef:f::/64";
      };
      ulaGateway = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "ULA gateway address.";
      };
      slaId = mkOption {
        type = types.nullOr types.int;
        default = null;
        description = "DHCPv6-PD sub-allocation index for border track6. Informational — hosts never reference the GUA prefix.";
      };
    };
  });
in {
  options.topology = mkOption {
    type = types.submodule {
      options.vlans = mkOption {
        type = types.attrsOf vlanType;
        default = {};
        description = "VLAN/network definitions keyed by name.";
      };
    };
    default = {};
    apply = validate;
    description = "Fleet-wide network topology.";
  };
}
