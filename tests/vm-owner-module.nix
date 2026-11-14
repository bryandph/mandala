{
  lib,
  pkgs,
}: let
  package = pkgs.writeShellScriptBin "mandala-vm-owner" "exit 0";
  evaluate = enabled:
    (lib.evalModules {
      specialArgs = {inherit pkgs;};
      modules = [
        ../modules/vm-owner.nix
        ({lib, ...}: {
          options = {
            environment.systemPackages = lib.mkOption {
              type = lib.types.listOf lib.types.package;
              default = [];
            };
            environment.etc = lib.mkOption {
              type = lib.types.attrsOf lib.types.raw;
              default = {};
            };
            systemd.tmpfiles.rules = lib.mkOption {
              type = lib.types.listOf lib.types.str;
              default = [];
            };
            assertions = lib.mkOption {
              type = lib.types.listOf lib.types.raw;
              default = [];
            };
          };
        })
        (lib.optionalAttrs enabled {
          mandala.vmOwner = {
            enable = true;
            owner = "host-a";
            inherit package;
          };
        })
      ];
    }).config;
  disabled = evaluate false;
  enabled = evaluate true;
in
  assert disabled.environment.systemPackages == [];
  assert disabled.systemd.tmpfiles.rules == [];
  assert disabled.environment.etc == {};
  assert enabled.environment.systemPackages == [package];
  assert enabled.systemd.tmpfiles.rules == ["d /var/lib/mandala/vm 0700 root root -"];
  assert lib.all (assertion: assertion.assertion) enabled.assertions; true
