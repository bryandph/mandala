# Ansible inventory flakeModule — wiring + hook options ONLY; the
# projection is lib.projections.ansibleInventory. Importing this gives the
# flake `flake.ansibleInventory` (serve it live via a dynamic inventory
# script that evals it) and contributes the result to
# flake.mandala.projections.
{
  config,
  lib,
  ...
}: let
  engine = import ../lib {inherit lib;};
  cfg = config.mandala.ansible;
in {
  imports = [./fleet.nix];

  options.mandala.ansible = {
    extraHostvars = lib.mkOption {
      type = lib.types.functionTo (lib.types.attrsOf lib.types.anything);
      default = _name: {};
      description = "Consumer hook: member name -> extra hostvars, merged after the engine defaults (so it can override them); the member's own deployment.ansible.vars merge last.";
    };
    pythonInterpreter = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = "/run/current-system/sw/bin/python3";
      description = "ansible_python_interpreter emitted for NixOS members; null omits the var.";
    };
    guardGroup = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = "deploy_rs";
      description = "Name of the synthetic guard group of deploy-rs-activatable members; null omits it.";
    };
  };

  config = {
    flake.ansibleInventory = engine.projections.ansibleInventory {
      hosts = config.mandala.members;
      inherit (cfg) extraHostvars pythonInterpreter guardGroup;
    };
    mandala.projections.ansibleInventory = config.flake.ansibleInventory;
  };
}
