# sops config flakeModule — wiring + hook options ONLY; validation is
# lib.evalSecrets, rendering is lib.projections.sopsConfig. Importing this
# and declaring secrets gives the flake `flake.sopsConfig` (the generated
# .sops.yaml content — render it with pkgs.formats.yaml and deliver it
# however the repo likes) and contributes the result to
# flake.mandala.projections.
#
# Recipients come from the members themselves (deployment.sops.recipient);
# the operator anchor is the one hook value, because it lives in operator
# data, not member data.
{
  config,
  lib,
  ...
}: let
  engine = import ../lib {inherit lib;};
  cfg = config.mandala.sops;
in {
  imports = [./fleet.nix];

  options.mandala.sops = {
    operatorAnchor = lib.mkOption {
      type = lib.types.str;
      description = "Operator PGP fingerprint, present in every creation rule's pgp group.";
    };
    declarations = lib.mkOption {
      type = lib.types.lazyAttrsOf lib.types.raw;
      default = {};
      description = "Secret-grade declarations (schema/secrets.nix data), validated against the fleet by lib.evalSecrets.";
    };
  };

  config = lib.mkIf (cfg.declarations != {}) {
    flake.sopsConfig = engine.projections.sopsConfig {
      operatorAnchor = cfg.operatorAnchor;
      recipients =
        lib.mapAttrs (_: m: m.deployment.sops.recipient)
        (lib.filterAttrs (_: m: m.deployment.sops.recipient != null) config.mandala.members);
      secrets = engine.evalSecrets {
        inherit (cfg) declarations;
        hosts = config.mandala.members;
      };
    };
    mandala.projections.sopsConfig = config.flake.sopsConfig;
  };
}
