# PKI trust-anchor schema: certificate authorities keyed by name.
# Facts only — PUBLIC certificates; private keys never enter the
# contract. `signedBy` references another CA by attribute name and is
# validated at eval time (enum over the declared names), so a dangling
# chain reference fails the consumer's eval.
{
  config,
  lib,
  ...
}: let
  inherit (lib) mkOption types;

  caNames = builtins.attrNames config.pki.cas;

  caType = types.submodule {
    options = {
      pem = mkOption {
        type = types.addCheck types.str (lib.hasPrefix "-----BEGIN CERTIFICATE-----");
        description = "PEM-encoded public certificate.";
      };
      kind = mkOption {
        type = types.enum ["root" "intermediate"];
        description = "Whether this CA is self-signed (root) or chained (intermediate).";
      };
      signedBy = mkOption {
        type = types.nullOr (types.enum caNames);
        default = null;
        description = "Name of the CA (in this set) that signed this one; null for roots.";
      };
      description = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Human-readable label.";
      };
    };
  };
in {
  options.pki.cas = mkOption {
    type = types.attrsOf caType;
    default = {};
    description = "Certificate authorities keyed by name.";
  };
}
