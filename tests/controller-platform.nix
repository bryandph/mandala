# Standalone synthetic contract test; no fleet, keys, derivations or runtime IO.
# Run with the repository's locked nixpkgs lib supplied as `lib`.
{lib}: let
  engine = import ../lib {inherit lib;};
  member = name:
    engine.evalMember {
      inherit name;
      tags = lib.optional (name == "grouped") "edge";
      deployment = {
        ansible.enable = true;
        deployRs = {
          enable = true;
          identityFile =
            if name == "member"
            then "/keys/member"
            else null;
        };
      };
    };
  members = lib.genAttrs ["plain" "grouped" "member"] member;
  nixosConfigurations =
    lib.mapAttrs (_: host: {
      config = {
        inherit host;
        system.build.toplevel = "/synthetic/profile";
      };
      pkgs.stdenv.hostPlatform.system = "x86_64-linux";
    })
    members;
  eval = deployment:
    (lib.evalModules {
      specialArgs.inputs = {
        self = {inherit nixosConfigurations;};
        deploy-rs.lib.x86_64-linux = {};
        nixpkgs = ./fixtures/deploy-nixpkgs.nix;
      };
      modules = [
        ../flake-modules/ansible.nix
        ../flake-modules/deploy.nix
        {
          options = {
            flake = lib.mkOption {type = lib.types.lazyAttrsOf lib.types.raw;};
            perSystem = lib.mkOption {type = lib.types.raw;};
          };
          config.mandala = {inherit deployment;};
        }
      ];
    }).config;
  defaults = {
    settings.identityFile = "/Users/bryan/.ssh/id_rsa";
    groupSettings.edge.identityFile = "/keys/group";
  };
  baseline = eval defaults;
  configured = eval (defaults
    // {
      controllerSettings = {
        darwin.identityFile = "/Users/bryan/.ssh/id_rsa";
        linux.identityFile = "/home/bryan/.ssh/id_rsa";
      };
    });
  projections = configured.flake.mandala.projections;
  darwin = projections.byControllerPlatform.darwin;
  linux = projections.byControllerPlatform.linux;
  fails = value: !(builtins.tryEval (builtins.deepSeq value true)).success;
  expectedSettings = name: {
    activation = "switch";
    autoRollback = true;
    fastConnection = true;
    hostname = name;
    sshPort = 22;
    sshUser = "root";
    identityFile =
      if name == "grouped"
      then "/keys/group"
      else if name == "member"
      then "/keys/member"
      else "/Users/bryan/.ssh/id_rsa";
  };
  # Independent pre-map byte shape, not a second run of the new selector.
  expectedInventory = {
    all = {
      hosts =
        lib.mapAttrs (name: _: {
          ansible_host = name;
          ansible_user = "root";
          ansible_python_interpreter = "/run/current-system/sw/bin/python3";
          ansible_ssh_private_key_file = (expectedSettings name).identityFile;
          ansible_ssh_common_args = "-o IdentitiesOnly=yes -o IdentityAgent=none";
        })
        members;
      children = lib.genAttrs ["nixos" "x86_64" "server" "edge" "deploy_rs"] (group: {
        hosts = lib.genAttrs (
          if group == "edge"
          then ["grouped"]
          else lib.attrNames members
        ) (_: null);
      });
    };
  };
  # Mock only the derivation-producing toolchain; exercise the real node
  # projection and its strict connection argv/settings whitelist unchanged.
  nodes = settings:
    engine.projections.deployNodes {
      deploy-rs.lib.x86_64-linux = {};
      nixpkgs = ./fixtures/deploy-nixpkgs.nix;
      inherit nixosConfigurations;
      deploySettings = settings;
    };
  oldNode = (nodes baseline.flake.mandala.projections.deploy.settings).plain;
  darwinNode = (nodes darwin.deploy.settings).plain;
  linuxNode = (nodes linux.deploy.settings).plain;
in
  assert builtins.toJSON baseline.flake.ansibleInventory == builtins.toJSON expectedInventory;
  assert builtins.toJSON baseline.flake.mandala.projections.deploy.settings
  == builtins.toJSON (lib.genAttrs (lib.attrNames members) expectedSettings);
  assert !(baseline.flake.mandala.projections ? byControllerPlatform);
  assert builtins.toJSON configured.flake.ansibleInventory == builtins.toJSON baseline.flake.ansibleInventory;
  assert builtins.toJSON darwin.deploy.settings == builtins.toJSON baseline.flake.mandala.projections.deploy.settings;
  assert builtins.toJSON darwin.ansibleInventory == builtins.toJSON baseline.flake.ansibleInventory;
  assert linux.deploy.settings.plain.identityFile == "/home/bryan/.ssh/id_rsa";
  assert linux.deploy.settings.grouped.identityFile == "/keys/group";
  assert linux.deploy.settings.member.identityFile == "/keys/member";
  assert linux.ansibleInventory.all.hosts.plain.ansible_ssh_private_key_file == linux.deploy.settings.plain.identityFile;
  assert linux.ansibleInventory.all.hosts.grouped.ansible_ssh_private_key_file == "/keys/group";
  assert linux.ansibleInventory.all.hosts.member.ansible_ssh_private_key_file == "/keys/member";
  assert linux.ansibleInventory.all.hosts.plain.ansible_ssh_common_args == "-o IdentitiesOnly=yes -o IdentityAgent=none";
  assert configured.flake.ansibleInventoryByControllerPlatform.linux == linux.ansibleInventory;
  assert builtins.toJSON darwinNode == builtins.toJSON oldNode;
  assert linuxNode.sshOpts == ["-p" "22" "-i" "/home/bryan/.ssh/id_rsa" "-o" "IdentitiesOnly=yes" "-o" "IdentityAgent=none"];
  assert configured.flake.deployByControllerPlatform.linux.nodes.plain == linuxNode;
  assert configured.flake.deployByControllerPlatform.darwin.nodes.plain == oldNode;
  assert !(linuxNode ? identityFile) && !(linuxNode ? controllerSettings);
  assert !(lib.hasInfix "/Users/" (builtins.toJSON linuxNode));
  assert fails (eval {controllerSettings.linux.identityFile = "relative/key";}).flake.mandala;
  assert fails (eval {controllerSettings.darwin.identityFile = "~/key";}).flake.mandala;
  assert fails (eval {controllerSettings.freebsd.identityFile = "/keys/test";}).flake.mandala;
  assert fails (eval {controllerSettings.linux.sshUser = "unexpected";}).flake.mandala; {
    noMapByteParity = true;
    darwinByteParity = true;
    linuxIdentity = true;
    groupMemberPrecedence = true;
    strictKeyPolicy = true;
    deployRsNoStaleIdentity = true;
    absolutePathValidation = true;
    unsupportedAuthoringRejected = true;
  }
