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
      hostname = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "SSH endpoint override; null inherits the next tier and ultimately the member FQDN.";
      };
      sshUser = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "SSH connection user; null inherits the next tier and ultimately root.";
      };
      sshPort = lib.mkOption {
        type = lib.types.nullOr lib.types.port;
        default = null;
        description = "SSH connection port; null inherits the next tier and ultimately 22.";
      };
      identityFile = lib.mkOption {
        type = lib.types.nullOr (lib.types.strMatching "/.*");
        default = null;
        description = "Absolute path string to the SSH private-key file; passed to clients without importing the key into the Nix store.";
      };
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
    # The old member-only connection surface remains a compatibility input.
    # Its schema defaults are not authored overrides: otherwise root/22/fqdn
    # would permanently mask a fleet or group setting. A member that needs to
    # override a layered value back to one of those defaults uses the explicit
    # deployment.deployRs scalar.
    legacyMemberConnection =
      lib.optionalAttrs (deployment.ssh.host != member.fqdn) {
        hostname = deployment.ssh.host;
      }
      // lib.optionalAttrs (deployment.ssh.user != "root") {
        sshUser = deployment.ssh.user;
      }
      // lib.optionalAttrs (deployment.ssh.port != 22) {
        sshPort = deployment.ssh.port;
      };
    memberSettings =
      legacyMemberConnection
      // lib.filterAttrs
      (_: value: value != null)
      (removeAttrs deployment.deployRs ["activation" "enable"])
      // {
        inherit (deployment.deployRs) sshOpts;
      };
    merged = engine.mergeDeploySettings {
      knownGroups = knownDeploymentGroups;
      fleet = cfg.deployment.settings;
      groupSettings = cfg.deployment.groupSettings;
      memberGroups = engine.ansibleGroupsFor member;
      member = memberSettings;
    };
  in
    {
      activation = deployment.deployRs.activation;
      hostname = member.fqdn;
      sshUser = "root";
      sshPort = 22;
    }
    // merged;
  flattenedDeploySettings =
    lib.mapAttrs
    (_: deploySettingsFor)
    cfg.members;
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
