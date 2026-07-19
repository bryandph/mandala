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
  rootsWithSigner = lib.filter (name: let ca = config.pki.cas.${name}; in ca.kind == "root" && ca.signedBy != null) caNames;
  unsignedIntermediates = lib.filter (name: let ca = config.pki.cas.${name}; in ca.kind == "intermediate" && ca.signedBy == null) caNames;
  unknownSigners = lib.filter (name: let signer = config.pki.cas.${name}.signedBy; in signer != null && !(config.pki.cas ? ${signer})) caNames;
  hasCycle = current: visited:
    if lib.elem current visited
    then true
    else let signer = config.pki.cas.${current}.signedBy; in signer != null && config.pki.cas ? ${signer} && hasCycle signer (visited ++ [current]);
  cyclicCas = lib.filter (name: hasCycle name []) caNames;

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

  config.assertions = [
    {
      assertion = rootsWithSigner == [];
      message = "PKI roots must not declare signedBy: ${lib.concatStringsSep ", " rootsWithSigner}";
    }
    {
      assertion = unsignedIntermediates == [];
      message = "PKI intermediates must declare signedBy: ${lib.concatStringsSep ", " unsignedIntermediates}";
    }
    {
      assertion = unknownSigners == [];
      message = "PKI CAs reference unknown signers: ${lib.concatStringsSep ", " unknownSigners}";
    }
    {
      assertion = cyclicCas == [];
      message = "PKI signer graph contains a cycle involving: ${lib.concatStringsSep ", " cyclicCas}";
    }
  ];
}
