# Managed fake-fleet parity, including direct deploy-rs node JSON. Only the
# derivation-producing toolchain is mocked, as in controller-platform.nix.
{lib}: let
  engine = import ../../lib {inherit lib;};
  host = engine.evalMember (lib.recursiveUpdate (import ../../examples/fake-fleet/member.nix) {
    build = {
      system = "x86_64-linux";
      buildPlatform = null;
    };
    deployment = {
      ansible.enable = true;
      deployRs.enable = true;
    };
  });
  evaluated =
    (lib.evalModules {
      specialArgs.inputs = {
        self.nixosConfigurations.${host.name} = {
          config = {
            inherit host;
            system.build.toplevel = "/synthetic/profile";
          };
          pkgs.stdenv.hostPlatform.system = "x86_64-linux";
        };
        deploy-rs.lib.x86_64-linux = {};
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
          config.mandala.extraMembers.facts-only = engine.evalMember {name = "facts-only";};
        }
      ];
    }).config;
in {
  aggregate = evaluated.flake.mandala;
  deploy = evaluated.flake.deploy;
  inventory = evaluated.flake.ansibleInventory;
}
