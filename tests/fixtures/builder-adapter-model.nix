{lib}: let
  f = import ./builders.nix {inherit lib;};
in
  f.configured.flake.mandala.projections.builders.byController
  // {
    empty = {
      buildMachines = [];
      ssh = {};
      knownHosts = {};
    };
  }
