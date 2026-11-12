{lib}: let
  baseline = import ./fixtures/builder-legacy.nix {inherit lib;};
  managed = import ./fixtures/builder-managed-legacy.nix {inherit lib;};
  f = import ./fixtures/builders.nix {inherit lib;};
  absent = f.eval {};
  facetOnly = f.eval {inherit (f) members;};
  empty = f.eval {
    inherit (f) members;
    controllers.empty = {
      selfMember = "wsl";
      allowedBuilders = [];
    };
  };
  configured = f.configured;
  deployment = {
    settings.identityFile = "/keys/deploy-fleet";
    controllerSettings = {
      linux.identityFile = "/keys/deploy-linux";
      darwin.identityFile = "/keys/deploy-mac";
    };
  };
  deployOnly = f.eval {
    inherit (f) members;
    inherit deployment;
  };
  mixed = f.eval {
    inherit (f) members controllers;
    inherit deployment;
  };
  collision =
    (lib.evalModules {
      specialArgs.inputs.self.nixosConfigurations.builder.config.host = f.members.builder;
      modules = [
        ../flake-modules/fleet.nix
        {
          options.flake = lib.mkOption {type = lib.types.lazyAttrsOf lib.types.raw;};
          config.mandala.extraMembers.builder = f.members.builder;
        }
      ];
    }).config.flake.mandala;
in
  assert lib.all (name: builtins.toJSON baseline.${name} == builtins.readFile (./fixtures/builder-baseline + "/${name}.json")) ["aggregate" "deploy" "inventory"];
  assert lib.all (name: builtins.toJSON managed.${name} == builtins.readFile (./fixtures/builder-baseline + "/managed-${name}.json")) ["aggregate" "deploy" "inventory"];
  assert absent.flake.buildMachinesByController == {};
  assert !(absent.flake.mandala.projections ? builders);
  assert !(facetOnly.flake.mandala.projections ? builders);
  assert facetOnly.flake.mandala.members.builder.builder != null;
  assert !(facetOnly.flake.mandala.members.wsl ? builder);
  assert empty.flake.buildMachinesByController.empty == [];
  assert empty.flake.mandala.projections.builders.byController.empty
  == {
    buildMachines = [];
    ssh = {};
    knownHosts = {};
  };
  assert builtins.toJSON configured.flake.mandala.projections.deploy == builtins.toJSON facetOnly.flake.mandala.projections.deploy;
  assert configured.flake.ansibleInventory == facetOnly.flake.ansibleInventory;
  assert mixed.flake.mandala.projections.deploy == deployOnly.flake.mandala.projections.deploy;
  assert mixed.flake.mandala.projections.byControllerPlatform == deployOnly.flake.mandala.projections.byControllerPlatform;
  assert mixed.flake.buildMachinesByController == configured.flake.buildMachinesByController;
  assert f.fails collision; true
