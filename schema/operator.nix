# Operator identity schema. Authored once in the operator-data flake
# (e.g. mandala-bph); the short GPG key-id forms are DERIVED from the
# full fingerprint so only one representation is ever hand-authored.
{
  config,
  lib,
  ...
}: let
  inherit (lib) mkOption types;
  cfg = config.operator;
in {
  options.operator = {
    name = mkOption {
      type = types.str;
      description = "Primary account/login name of the operator.";
    };

    fullname = mkOption {
      type = types.str;
      description = "Operator display name.";
    };

    email = mkOption {
      type = types.str;
      description = "Operator email address.";
    };

    gpg = {
      fingerprint = mkOption {
        type = types.strMatching "[0-9A-F]{40}";
        description = ''
          Full 40-hex-char OpenPGP v4 fingerprint (uppercase, no spaces).
          The only hand-authored representation; key ids derive from it.
        '';
      };

      keyIdLong = mkOption {
        type = types.str;
        readOnly = true;
        default = lib.substring 24 16 cfg.gpg.fingerprint;
        defaultText = "last 16 hex chars of operator.gpg.fingerprint";
        description = "Derived 64-bit (long) GPG key id.";
      };

      keyIdShort = mkOption {
        type = types.str;
        readOnly = true;
        default = lib.substring 32 8 cfg.gpg.fingerprint;
        defaultText = "last 8 hex chars of operator.gpg.fingerprint";
        description = "Derived 32-bit (short) GPG key id.";
      };

      openpgp4fpr = mkOption {
        type = types.str;
        readOnly = true;
        default = "openpgp4fpr:${cfg.gpg.fingerprint}";
        defaultText = "openpgp4fpr:<operator.gpg.fingerprint>";
        description = "Derived openpgp4fpr URI (DNS TXT key-discovery form).";
      };

      publicKeyUrl = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Where the ASCII-armored public key is published (PKA uri field).";
      };
    };

    sshPublicKeys = mkOption {
      type = types.listOf types.str;
      default = [];
      description = "Authorized SSH public keys, one per element.";
    };
  };
}
