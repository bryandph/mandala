{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.mandala.vmOwner;
in {
  options.mandala.vmOwner = {
    enable = lib.mkEnableOption "the owner-scoped VM SSH stdio helper (no daemon or listener)";
    owner = lib.mkOption {
      type = lib.types.str;
      description = "Canonical fleet member identity; the helper refuses requests for a different owner.";
    };
    package = lib.mkOption {
      type = lib.types.package;
      description = "Mandala package containing mandala-vm-owner. Explicitly supplied by the consumer; no extra flake input.";
    };
  };
  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = pkgs.stdenv.isLinux;
        message = "mandala VM owner application supports NixOS owners only";
      }
    ];
    environment.systemPackages = [cfg.package];
    environment.etc."mandala/vm-owner.json".text = builtins.toJSON {inherit (cfg) owner;};
    systemd.tmpfiles.rules = ["d /var/lib/mandala/vm 0700 root root -"];
  };
}
