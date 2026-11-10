{
  config,
  lib,
  ...
}: let
  cfg = config.mandala.builders;
  profile = cfg.byController.${cfg.controller} or (throw "mandala builders: unknown controller '${cfg.controller}'");
  render = import ../lib/render-builders.nix {inherit lib;};
in {
  options.mandala.builders = {
    enable = lib.mkEnableOption "the explicitly selected distributed builder profile";
    controller = lib.mkOption {
      type = lib.types.str;
      description = "Explicit controller ID in byController; no runtime or OS inference.";
    };
    byController = lib.mkOption {
      type = lib.types.attrsOf lib.types.raw;
      default = {};
      description = "Precomputed mandala.projections.builders.byController from the data flake.";
    };
    exclusiveBuildMachines = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Discard other modules' nix.buildMachines contributions. Forces only that option; review duplicate capacity before enabling.";
    };
    rendered = lib.mkOption {
      type = lib.types.attrsOf lib.types.raw;
      readOnly = true;
      description = "Pure selected SSH/trust/machines-file inspection, not activated daemon state.";
    };
  };
  config = lib.mkIf cfg.enable {
    mandala.builders.rendered = render profile;
    nix.distributedBuilds = true;
    nix.settings.builders-use-substitutes = true;
    nix.buildMachines =
      if cfg.exclusiveBuildMachines
      then lib.mkForce profile.buildMachines
      else profile.buildMachines;
    programs.ssh.extraConfig = lib.mkBefore cfg.rendered.sshConfig;
    programs.ssh.knownHosts = cfg.rendered.knownHosts;
  };
}
