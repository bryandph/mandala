# Secret-grade secrets declarations. Each named secret states which sops
# file holds it, who may read it, and where its value originates — contract
# data the .sops.yaml projection renders and rotation work orders query
# ("what breaks if I rotate X"). Declarations are AUTHORED in consumer
# repos (paths are repo-relative facts, like members are fleet facts); the
# public engine holds only this schema and fictional example data.
#
# Readers resolve against the fleet in `lib.evalSecrets`, which enforces
# the cross-field invariants the module system can't express per-option:
# every resolved reader has a sops recipient (a reader without one is an
# EVAL FAILURE, never a silent omission from the creation rule), referenced
# groups are non-empty, adminOnly excludes other readers, paths are unique.
{lib, ...}: let
  inherit (lib) mkOption types;
in {
  options.secrets = mkOption {
    type = types.attrsOf (types.submodule {
      options = {
        path = mkOption {
          type = types.str;
          description = "Repo-relative sops file holding this secret. Unique across declarations — the creation rule is generated from it.";
          example = "secrets/wifi.yaml";
        };

        readers = {
          members = mkOption {
            type = types.listOf types.str;
            default = [];
            description = "Member names whose age recipients decrypt this secret.";
            example = ["uconsole" "adsb"];
          };
          groups = mkOption {
            type = types.listOf types.str;
            default = [];
            description = "Taxonomy groups (lib.groupsFor spelling) whose members decrypt this secret. Group membership resolves at eval time, so the reader set tracks the fleet.";
            example = ["k3s"];
          };
          all = mkOption {
            type = types.bool;
            default = false;
            description = "Every member with a sops identity decrypts this secret.";
          };
          adminOnly = mkOption {
            type = types.bool;
            default = false;
            description = "Seal to the operator anchor alone — the creation rule carries NO member recipients. Mutually exclusive with every other reader field.";
          };
        };

        custody = {
          keySource = mkOption {
            type = types.enum ["operator" "generated" "external"];
            default = "operator";
            description = "Where the secret value originates: hand-authored by the operator, minted by fleet tooling (e.g. per-host age keys), or issued by an external party. Rotation work orders dispatch on this.";
          };
          note = mkOption {
            type = types.nullOr types.str;
            default = null;
            description = "Free-form custody note (issuer, rotation cadence, console URL, …).";
          };
        };
      };
    });
    default = {};
    description = "Named secret declarations, keyed by secret name.";
  };
}
