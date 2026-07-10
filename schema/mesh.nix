# Overlay-mesh membership schema (ZeroTier-shaped). The overlay is ONE
# central table, like topology.vlans — a member id is assigned by the
# overlay controller (an external authority), so it is never derivable
# from any box's own configuration and is authored exactly once here,
# NOT per-member. Entries are keyed by the consumer's stable resource
# key (terraform state addresses hang off it — renaming a key is a
# destroy/create unless you move state).
{lib, ...}: let
  inherit (lib) mkOption types;
in {
  options.mesh = {
    members = mkOption {
      type = types.attrsOf (types.submodule {
        options = {
          memberId = mkOption {
            type = types.strMatching "[0-9a-f]{10}";
            description = "Overlay member id (10 hex chars, controller-assigned).";
          };
          ip = mkOption {
            type = types.str;
            description = "Assigned overlay IPv4 address.";
            example = "10.16.42.29";
          };
          ip6 = mkOption {
            type = types.nullOr types.str;
            default = null;
            description = "Assigned overlay IPv6 (ULA) address; null = v4-only member.";
            example = "fd42:cafe:feed:2a::29";
          };
          name = mkOption {
            type = types.nullOr types.str;
            default = null;
            description = "Display name registered with the controller; projections fall back to the entry key when null.";
          };
          dnsName = mkOption {
            type = types.nullOr types.str;
            default = null;
            description = "Relative DNS name (within the operator's internal zone) that resolves to the overlay address — drives the mesh DNS-record projection. null = no mesh DNS record.";
            example = "ash.hzn";
          };
        };
      });
      default = {};
      description = "Overlay members keyed by stable resource key.";
    };
  };
}
