# deploy-rs flakeModule — wiring ONLY; the projections are
# lib.projections.{deployNodes,deployBatch}. Importing this gives the
# flake `flake.deploy.nodes` plus `legacyPackages.<system>.deployBatch`
# (the eval-once per-group batch artifact), and contributes the node list
# to flake.mandala.projections.
#
# Consumer-input conventions: this module resolves `inputs.deploy-rs` and
# `inputs.nixpkgs` from the CONSUMER's flake — the engine pins neither
# toolchain. The consuming flake must declare a `deploy-rs` input (it owns
# the version; same pattern as terranix consumption via _module.args).
{
  config,
  lib,
  inputs,
  ...
}: let
  engine = import ../lib {inherit lib;};
in {
  imports = [./fleet.nix];

  config = {
    flake.deploy.nodes = engine.projections.deployNodes {
      inherit (inputs) deploy-rs nixpkgs;
      nixosConfigurations = inputs.self.nixosConfigurations or {};
    };

    # The data view for the aggregate: node NAMES only — the nodes
    # themselves carry derivations and must not enter flake.mandala.
    mandala.projections.deploy.nodes = lib.attrNames config.flake.deploy.nodes;

    perSystem = {
      pkgs,
      system,
      ...
    }: {
      legacyPackages.deployBatch = engine.projections.deployBatch {
        inherit pkgs;
        deployLib = inputs.deploy-rs.lib.${system};
        inherit (inputs.self) deploy;
        hosts = config.mandala.members;
      };
    };
  };
}
