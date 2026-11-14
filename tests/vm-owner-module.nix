{
  lib,
  pkgs,
}: let
  package = pkgs.writeShellScriptBin "mandala-vm-owner" "exit 0";
  # Owner-platform assertions must not depend on the controller running this check.
  evaluate = isLinux: enabled:
    (lib.evalModules {
      specialArgs.pkgs = pkgs // {stdenv = pkgs.stdenv // {inherit isLinux;};};
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
  disabled = evaluate false false;
  enabled = evaluate true true;
  unsupported = evaluate false true;
in
  assert disabled.environment.systemPackages == [];
  assert disabled.systemd.tmpfiles.rules == [];
  assert disabled.environment.etc == {};
  assert disabled.assertions == [];
  assert enabled.environment.systemPackages == [package];
  assert enabled.systemd.tmpfiles.rules == ["d /var/lib/mandala/vm 0700 root root -"];
  assert lib.all (assertion: assertion.assertion) enabled.assertions;
  assert builtins.length unsupported.assertions == 1;
  assert (builtins.head unsupported.assertions).assertion == false;
  assert (builtins.head unsupported.assertions).message == "mandala VM owner application supports NixOS owners only"; true
