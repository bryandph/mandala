# Core fleet flakeModule — wiring + hook options ONLY; all logic lives in
# lib/ (lib.aggregate). Import this (the tool modules import it for you)
# and the flake gains `flake.mandala`: the versioned aggregate contract
# {schemaVersion, members, groups, projections} that porcelain (CLI/TUI,
# plugged engines) reads in ONE eval. Everything in it is pure data —
# `nix eval --json .#mandala` must never instantiate a derivation.
#
# Consumer conventions (the contract a consuming flake follows):
#   - NixOS members: expose `flake.nixosConfigurations` whose modules
#     import the member schema (mandala.lib.schemas.member) and author
#     `host.*`. Their validated `config.host` IS the member.
#   - Non-NixOS / facts-only members: pass VALIDATED members (evalMember
#     results — e.g. an operator-data flake's `data.members`) via
#     `mandala.extraMembers`. Namespaces must not overlap.
#   - Operator data (optional): `mandala.operator` takes VALIDATED
#     operator data (lib.evalOperator yours, or pass an operator-data
#     flake's data.operator) into `flake.mandala.operator`.
{
  config,
  lib,
  inputs,
  ...
}: let
  engine = import ../lib {inherit lib;};
  cfg = config.mandala;
  nixosMembers = lib.mapAttrs (_: c: c.config.host) (inputs.self.nixosConfigurations or {});
  collisions = lib.intersectLists (lib.attrNames nixosMembers) (lib.attrNames cfg.extraMembers);
  mergedMembers = nixosMembers // cfg.extraMembers;
  validHostname = name:
    builtins.stringLength name
    <= 63
    && builtins.match "[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?" name != null
    && lib.toLower name != "all";
  invalidKeys = lib.filter (name: !validHostname name) (lib.attrNames mergedMembers);
  mismatchedNames = lib.filter (name: !(mergedMembers.${name} ? name) || mergedMembers.${name}.name != name) (lib.attrNames mergedMembers);
in {
  options.mandala = {
    extraMembers = lib.mkOption {
      type = lib.types.lazyAttrsOf lib.types.raw;
      default = {};
      description = "Validated non-NixOS members (lib.evalMember results) merged into the fleet alongside nixosConfigurations' host metadata.";
    };

    members = lib.mkOption {
      type = lib.types.lazyAttrsOf lib.types.raw;
      readOnly = true;
      description = "The merged member view (nixosConfigurations' config.host // extraMembers) — what every projection and tool module reads.";
    };

    operator = lib.mkOption {
      type = lib.types.nullOr lib.types.raw;
      default = null;
      description = "VALIDATED operator data (lib.evalOperator result, or an operator-data flake's data.operator), carried into flake.mandala.operator.";
    };

    projections = lib.mkOption {
      type = lib.types.lazyAttrsOf lib.types.raw;
      default = {};
      description = "Serializable projection results, contributed by the tool flakeModules; carried into flake.mandala.projections.";
    };
  };

  config = {
    mandala.members = assert lib.assertMsg (collisions == [])
    "mandala member sources overlap: ${lib.concatStringsSep ", " collisions}";
    assert lib.assertMsg (invalidKeys == [])
    "mandala member keys must be bare RFC 1123 hostnames: ${lib.concatStringsSep ", " invalidKeys}";
    assert lib.assertMsg (mismatchedNames == [])
    "mandala member keys must equal host.name: ${lib.concatStringsSep ", " mismatchedNames}"; mergedMembers;

    flake.mandala = engine.aggregate {
      inherit (cfg) members projections operator;
    };
  };
}
