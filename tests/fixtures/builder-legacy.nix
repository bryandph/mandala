# This fixture is evaluated unchanged before and after adding builder defaults.
{lib}: let
  engine = import ../../lib {inherit lib;};
  host = engine.evalMember (import ../../examples/fake-fleet/member.nix);
  evaluated =
    (lib.evalModules {
      specialArgs.inputs.self.nixosConfigurations = {};
      modules = [
        ../../flake-modules/ansible.nix
        {
          options.flake = lib.mkOption {type = lib.types.lazyAttrsOf lib.types.raw;};
          config.mandala.extraMembers.${host.name} = host;
        }
      ];
    }).config;
in {
  aggregate = evaluated.flake.mandala;
  deploy = evaluated.flake.mandala.projections.deploy;
  inventory = evaluated.flake.ansibleInventory;
}
