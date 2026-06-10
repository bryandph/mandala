# Network topology schema: VLAN/network definitions keyed by name.
# Authored once in the operator-data flake; consumers read
# `topology.vlans.<name>.*`.
#
# The IPv4 prefix length DERIVES from the subnet CIDR — only the CIDR is
# hand-authored. Entries may be id-only (subnet = null) for networks that
# exist on switch trunks but have no routed subnet authored yet.
{lib, ...}: let
  inherit (lib) mkOption types;

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
          else lib.toInt (lib.last (lib.splitString "/" config.subnet));
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
    description = "Fleet-wide network topology.";
  };
}
