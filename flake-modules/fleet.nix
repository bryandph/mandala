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
  deploySettingsType = lib.types.submodule {
    options = {
      autoRollback = lib.mkOption {
        type = lib.types.nullOr lib.types.bool;
        default = null;
        description = "Whether deploy-rs should reactivate the previous profile when activation fails.";
      };
      fastConnection = lib.mkOption {
        type = lib.types.nullOr lib.types.bool;
        default = null;
        description = "Whether deploy-rs should copy the full closure instead of allowing target substitution.";
      };
      magicRollback = lib.mkOption {
        type = lib.types.nullOr lib.types.bool;
        default = null;
        description = "Whether deploy-rs should require confirmation and roll back when the host cannot be reached.";
      };
      confirmTimeout = lib.mkOption {
        type = lib.types.nullOr lib.types.ints.u16;
        default = null;
        description = "Seconds deploy-rs waits for activation confirmation before rolling back.";
      };
      activationTimeout = lib.mkOption {
        type = lib.types.nullOr lib.types.ints.u16;
        default = null;
        description = "Seconds deploy-rs allows for profile activation.";
      };
      tempPath = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Remote directory deploy-rs uses for temporary activation state.";
      };
      sudo = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Command deploy-rs uses to execute activation as another user.";
      };
      user = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Remote user whose profile deploy-rs activates.";
      };
      sshOpts = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [];
        description = "Additional arguments passed to ssh by deploy-rs.";
      };
    };
  };
  knownDeploymentGroups =
    lib.unique (lib.concatMap engine.ansibleGroupsFor (lib.attrValues mergedMembers));
  unknownDeploymentGroups =
    lib.filter
    (name: !(lib.elem name knownDeploymentGroups))
    (lib.attrNames cfg.deployment.groupSettings);
  deploySettingsFor = member: let
    deployment = member.deployment;
    memberSettings =
      (removeAttrs deployment.deployRs ["activation" "enable"])
      // {
        sshOpts =
          ["-p" (toString deployment.ssh.port)]
          ++ deployment.deployRs.sshOpts;
      };
  in
    {
      activation = deployment.deployRs.activation;
      hostname = deployment.ssh.host;
      sshUser = deployment.ssh.user;
    }
    // engine.mergeDeploySettings {
      knownGroups = knownDeploymentGroups;
      fleet = cfg.deployment.settings;
      groupSettings = cfg.deployment.groupSettings;
      memberGroups = engine.ansibleGroupsFor member;
      member = memberSettings;
    };
  flattenedDeploySettings =
    lib.mapAttrs
    (_: deploySettingsFor)
    (lib.filterAttrs (_: member: member.deployment.deployRs.enable) cfg.members);
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

    deployment = {
      settings = lib.mkOption {
        type = deploySettingsType;
        default = {};
        description = "Fleet-wide deploy-rs setting defaults. Null scalars and empty sshOpts are omitted when settings are flattened.";
      };
      groupSettings = lib.mkOption {
        type = lib.types.lazyAttrsOf deploySettingsType;
        default = {};
        description = "Deploy-rs settings by sanitized fleet taxonomy group; keys that do not name a known group fail evaluation.";
      };
    };

    projections = lib.mkOption {
      type = lib.types.lazyAttrsOf (lib.types.lazyAttrsOf lib.types.raw);
      default = {};
      description = "Serializable projection results, merged by projection field across tool flakeModules and carried into flake.mandala.projections.";
    };
  };

  config = {
    mandala.projections.deploy.settings = flattenedDeploySettings;

    mandala.members = assert lib.assertMsg (collisions == [])
    "mandala member sources overlap: ${lib.concatStringsSep ", " collisions}";
    assert lib.assertMsg (invalidKeys == [])
    "mandala member keys must be bare RFC 1123 hostnames: ${lib.concatStringsSep ", " invalidKeys}";
    assert lib.assertMsg (mismatchedNames == [])
    "mandala member keys must equal host.name: ${lib.concatStringsSep ", " mismatchedNames}";
    assert lib.assertMsg (unknownDeploymentGroups == [])
    "mandala deployment.groupSettings keys must name known sanitized groups: ${lib.concatStringsSep ", " unknownDeploymentGroups}"; mergedMembers;

    flake.mandala = engine.aggregate {
      inherit (cfg) members projections operator;
    };
  };
}
