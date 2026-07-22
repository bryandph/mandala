# Class-agnostic fleet-member schema. A member is anything with an
# identity on the network: a NixOS host authors this in-config (its
# consumer flake imports this schema into the NixOS evaluation), while a
# non-NixOS member (router, switch, AP, Windows DC, BMC) is plain data in
# the operator flake, validated through `lib.evalMember`.
#
# Management surfaces (deployRs / ansible) default OFF, so a member that
# enables none of them is a facts-only inventory entry: it appears in
# DNS/docs projections and nothing pushes to it. A NixOS consumer flips
# the surfaces on at its configuration-factory layer (mkDefault), keeping
# this schema class-agnostic.
#
# Addressing carries ROLES, not a single "primary" flag — one member can
# take its DNS name from a mesh address, deploy over that mesh, and
# default-route via a different NIC. `assignment` is tri-state and gates
# which publication projections fire for the address.
{
  config,
  lib,
  ...
}: let
  inherit (lib) mkOption types;
  cfg = config.host;
  hostLabel = types.strMatching "[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?";
  withRole = role: lib.filter (network: lib.elem role network.roles) cfg.networks;
in {
  options.host = {
    name = mkOption {
      type = hostLabel;
      description = "Bare RFC 1123 hostname label (the primary member selector; FQDN is derived separately)";
      example = "ntp";
    };

    domain = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Domain name for the member";
      example = "servers.bph";
    };

    fqdn = mkOption {
      type = types.str;
      default =
        if cfg.domain != null
        then "${cfg.name}.${cfg.domain}"
        else cfg.name;
      readOnly = true;
      description = "Fully qualified domain name (derived from name and domain)";
    };

    category = mkOption {
      type = types.enum ["workstation" "server" "gateway" "appliance" "vm" "container"];
      default = "server";
      description = "Member category for organizational purposes";
    };

    role = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Primary role or purpose of the member";
      example = "dns";
    };

    location = mkOption {
      type = types.nullOr types.str;
      default = cfg.domain;
      defaultText = "config.host.domain";
      description = "Physical or logical location of the member";
      example = "servers.bph";
    };

    platform = mkOption {
      type = types.enum ["nixos" "darwin" "wsl" "aws" "hetzner" "gcp" "azure" "oci" "windows" "routeros" "opnsense" "firmware" "android"];
      default = "nixos";
      description = "Platform type (OS family or hosting venue)";
    };

    architecture = mkOption {
      type = types.enum ["x86_64" "aarch64" "armv7l"];
      default =
        if cfg.build != null
        then lib.head (lib.splitString "-" cfg.build.system)
        else "x86_64";
      defaultText = "first component of host.build.system, else x86_64";
      description = "CPU architecture (derived from host.build when present)";
    };

    description = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Human-readable description of the member";
    };

    tags = mkOption {
      type = types.listOf types.str;
      default = [];
      description = "Additional tags for the member";
      example = ["raspberry-pi" "edge-device"];
    };

    build = mkOption {
      type = types.nullOr (types.submodule {
        options = {
          system = mkOption {
            type = types.str;
            description = "Target system the member's closure is built for.";
            example = "aarch64-linux";
          };
          buildPlatform = mkOption {
            type = types.nullOr types.str;
            default = null;
            description = "When set, the closure is cross-compiled from this platform.";
            example = "aarch64-linux";
          };
        };
      });
      default = null;
      description = "How this member's system closure is built. Injected by the consumer's configuration factory for NixOS members; null for members with nothing to build (facts-only inventory entries).";
    };

    networks = mkOption {
      type = types.listOf (types.submodule {
        options = {
          vlan = mkOption {
            type = types.str;
            description = "Name referencing topology.vlans.<name>.";
            example = "servers";
          };
          id = mkOption {
            type = types.nullOr types.int;
            default = null;
            description = "Integer host identifier — the offset from the VLAN subnet's network address (id 102 on a /24 → .102; id 261 on 10.5.0.0/16 → 10.5.1.5). When set, lib.net derives address (subnet + id) and ula (the id as one decimal-literal group on the VLAN's ULA /64, e.g. id 102 → ::102) unless those are explicitly authored; realize via lib.evalMemberWith or lib.net.resolveNetworks.";
            example = 102;
          };
          address = mkOption {
            type = types.nullOr types.str;
            default = null;
            description = "IPv4 address on this network (no CIDR — prefix length derived from topology). Required when assignment = reservation. Derived from `id` when realized via lib.net.";
            example = "172.16.15.100";
          };
          ula = mkOption {
            type = types.nullOr types.str;
            default = null;
            description = "ULA IPv6 address (stable internal v6).";
            example = "fd42:cafe:feed:f::64";
          };
          interface = mkOption {
            type = types.nullOr types.str;
            default = null;
            description = "Physical NIC name. Required for managed networking on NixOS members.";
            example = "eth0";
          };
          mac = mkOption {
            type = types.nullOr (types.strMatching "[0-9a-f]{2}(:[0-9a-f]{2}){5}");
            default = null;
            description = "Hardware address (lowercase, colon-separated) — what a DHCP reservation is keyed by. null = not (yet) recorded; the dhcpReservations projection carries it through for reconciliation.";
            example = "dc:a6:32:aa:bb:cc";
          };
          roles = mkOption {
            type = types.listOf (types.enum ["dns" "reach" "gateway" "management"]);
            default = [];
            description = ''
              Jobs this attachment performs for the member:
              dns — the member's DNS name resolves to this address;
              reach — deploy/ansible ssh reaches the member here;
              gateway — default route (and, on NixOS, resolver/search-domain source);
              management — management-plane address (BMC, switch mgmt).
              At most one network may carry each role.
            '';
          };
          assignment = mkOption {
            type = types.enum ["static" "reservation" "dynamic"];
            default = "static";
            description = "How the address is assigned: static (member configures it itself), reservation (DHCP with a reserved address — drives the dhcpReservations projection), dynamic (plain DHCP lease, no address authored). Gates which publication projections fire.";
          };
        };
      });
      default = [];
      description = "Network attachments for this member. Each entry references a topology VLAN by name.";
    };

    deployment = {
      ssh = {
        host = mkOption {
          type = types.str;
          default = cfg.fqdn;
          defaultText = "config.host.fqdn";
          description = "SSH target (used as ansible_host and deploy-rs hostname)";
        };
        user = mkOption {
          type = types.str;
          default = "root";
          description = "SSH user for deployment tools";
        };
        port = mkOption {
          type = types.port;
          default = 22;
          description = "SSH port for deployment tools";
        };
      };

      deployRs = {
        enable = mkOption {
          type = types.bool;
          # Class-agnostic default: OFF. The NixOS configuration factory
          # mkDefaults this to true for every host it builds; non-NixOS
          # members stay facts-only unless they opt in.
          default = false;
          description = "Include this member in deploy-rs nodes";
        };
        activation = mkOption {
          type = types.enum ["switch" "boot"];
          default = "switch";
          description = "Activation mode: 'switch' for live activation, 'boot' for first-time deploys where switching would break the running system";
        };
        hostname = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "SSH endpoint override for deployment; null inherits group/fleet settings and ultimately host.fqdn";
        };
        sshUser = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "SSH connection user for deployment; null inherits group/fleet settings and ultimately root";
        };
        sshPort = mkOption {
          type = types.nullOr types.port;
          default = null;
          description = "SSH connection port for deployment; null inherits group/fleet settings and ultimately 22";
        };
        identityFile = mkOption {
          type = types.nullOr (types.strMatching "/.*");
          default = null;
          description = "Absolute path string to the SSH private-key file used for deployment; the path is passed to clients and is never imported into the Nix store";
        };
        autoRollback = mkOption {
          type = types.nullOr types.bool;
          default = null;
          description = "Whether deploy-rs should reactivate the previous profile when activation fails; null inherits group/fleet settings and ultimately the legacy true default";
        };
        fastConnection = mkOption {
          type = types.nullOr types.bool;
          default = null;
          description = "Whether deploy-rs should copy the full closure instead of allowing target substitution; null inherits group/fleet settings and ultimately the legacy true default";
        };
        magicRollback = mkOption {
          type = types.nullOr types.bool;
          default = null;
          description = "Whether deploy-rs should require confirmation and roll back when the host cannot be reached";
        };
        confirmTimeout = mkOption {
          type = types.nullOr types.ints.u16;
          default = null;
          description = "Seconds deploy-rs waits for activation confirmation before rolling back";
        };
        activationTimeout = mkOption {
          type = types.nullOr types.ints.u16;
          default = null;
          description = "Seconds deploy-rs allows for profile activation";
        };
        tempPath = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "Remote directory deploy-rs uses for temporary activation state";
        };
        sudo = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "Command deploy-rs uses to execute activation as another user";
        };
        user = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "Remote user whose profile deploy-rs activates";
        };
        sshOpts = mkOption {
          type = types.listOf types.str;
          default = [];
          description = "Additional arguments passed to ssh by deploy-rs";
        };
      };

      sops = {
        recipient = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "Public age recipient for this member (from ssh-to-age on the host's ssh ed25519 key). Drives the generated .sops.yaml. null = member has no sops identity.";
          example = "age1nvnnxzsl65d8p276yw8t09m5guq8the8flwdz7t73q6zqnwd5vvs9vmnv3";
        };
      };

      ansible = {
        enable = mkOption {
          type = types.bool;
          # Class-agnostic default: OFF — see deployRs.enable. The NixOS
          # factory turns it on; opt a host back out with an explicit
          # `host.deployment.ansible.enable = false`.
          default = false;
          description = "Include this member in the generated ansible inventory";
        };
        groups = mkOption {
          type = types.listOf types.str;
          default = [];
          description = "Extra groups beyond those derived from host.{platform,architecture,category,role,tags} — see lib.groupsFor";
          example = ["k8s_bph" "turing_pis"];
        };
        vars = mkOption {
          type = types.attrsOf types.anything;
          default = {};
          description = "Per-member ansible variables merged into the inventory's host_vars";
          example = {node_num = 2;};
        };
      };
    };
  };

  config.assertions = [
    {
      assertion = lib.toLower cfg.name != "all";
      message = "member ${cfg.name}: 'all' is reserved by the selector language";
    }
    {
      assertion =
        lib.all
        (role: lib.length (withRole role) <= 1)
        ["dns" "reach" "gateway" "management"];
      message = "member ${cfg.name}: at most one network may carry each address role";
    }
    {
      assertion = lib.all (network: network.assignment != "reservation" || network.address != null) cfg.networks;
      message = "member ${cfg.name}: assignment = \"reservation\" requires an address (it IS the reservation)";
    }
  ];
}
