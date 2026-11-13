# Fictional acme/globex fleet; no live systems or injected runtime toolchain.
{lib}: let
  engine = import ../../lib {inherit lib;};
  system = "x86_64-linux";
  ownerData = {
    name = "host-a";
    domain = "acme.example";
    tags = ["mixed"];
    build = {inherit system;};
    deployment.deployRs.enable = true;
    deployment.ansible.enable = true;
    vmApplication = {
      backend = "nixos-module";
      manifestVersion = 1;
      supportedSystems = [system "aarch64-linux"];
      guests = {
        guest-a = {inherit system;};
        guest-b.system = "aarch64-linux";
      };
    };
  };
  guestData = name: {
    inherit name;
    kind = "vm";
    category = "vm";
    domain = "acme.example";
    tags = ["guests-only" "mixed"];
    vm = {
      owner = "host-a";
      backend = "nixos-module";
    };
    build.system =
      if name == "guest-b"
      then "aarch64-linux"
      else system;
    networks = [
      {
        vlan = "services";
        address =
          if name == "guest-b"
          then "192.0.2.12"
          else "192.0.2.11";
        mac =
          if name == "guest-b"
          then "02:00:00:00:00:12"
          else "02:00:00:00:00:11";
        roles = ["dns"];
        assignment = "reservation";
      }
    ];
  };
  data = {
    host-a = ownerData;
    guest-a = guestData "guest-a";
    guest-b = guestData "guest-b";
    host-c = {
      name = "host-c";
      domain = "globex.example";
      build = {inherit system;};
      deployment.deployRs.enable = true;
      deployment.ansible.enable = true;
    };
    inventory = {
      name = "inventory";
      platform = "firmware";
    };
  };
  members = lib.mapAttrs (_: engine.evalMember) data;
  configurations = hosts:
    lib.mapAttrs (_: host: {
      config = {
        inherit host;
        # Deliberately poisonous: JSON evaluation must not force artifacts.
        system.build.toplevel = throw "VM contract forced a toplevel";
      };
      pkgs.stdenv.hostPlatform.system = host.build.system;
    }) (removeAttrs hosts ["inventory"]);
  eval = hosts:
    (lib.evalModules {
      specialArgs.inputs = {
        self.nixosConfigurations = configurations hosts;
        deploy-rs.lib.${system} = {};
        nixpkgs = ./deploy-nixpkgs.nix;
      };
      modules = [
        ../../flake-modules/ansible.nix
        ../../flake-modules/deploy.nix
        {
          options = {
            flake = lib.mkOption {type = lib.types.lazyAttrsOf lib.types.raw;};
            perSystem = lib.mkOption {type = lib.types.raw;};
          };
          config.mandala = {
            extraMembers.inventory = hosts.inventory;
            deployment.controllerSettings = {
              linux.identityFile = "/keys/operator-alice-linux";
              darwin.identityFile = "/keys/operator-alice-darwin";
            };
          };
        }
      ];
    }).config;
in {
  inherit engine system data members configurations eval ownerData guestData;
  fails = value: !(builtins.tryEval (builtins.deepSeq value true)).success;
  checked = engine.vm.validate {
    inherit members;
    configurationNames = lib.attrNames (configurations members);
  };
}
