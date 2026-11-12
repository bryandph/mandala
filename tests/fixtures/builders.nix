{lib}: let
  engine = import ../../lib {inherit lib;};
  pin = {
    kind = "pin";
    publicKey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
  };
  capacity = {
    systems = ["x86_64-linux" "aarch64-linux" "x86_64-linux"];
    supportedFeatures = ["kvm" "big-parallel" "kvm"];
    mandatoryFeatures = ["kvm"];
    sshUser = "builder";
    trust = pin;
    lanes = {
      wide = {maxJobs = 8;};
      serial = {
        maxJobs = 1;
        supportedFeatures = [];
        mandatoryFeatures = [];
        systems = ["x86_64-linux"];
      };
    };
  };
  members = lib.mapAttrs (name: data: engine.evalMember ({inherit name;} // data)) {
    wsl = {platform = "wsl";};
    linux = {};
    mac = {
      platform = "darwin";
      builder = capacity;
    };
    builder = {
      category = "server";
      build = null;
      builder = capacity;
    };
    workstation = {
      category = "workstation";
      builder = capacity // {lanes = {};};
    };
  };
  route = {identityFile = "/keys/operator-alice";};
  controllers = {
    wsl = {
      selfMember = "wsl";
      allowedBuilders = ["workstation" "builder"];
      routes = {
        workstation = route;
        builder =
          route
          // {
            hostname = "build-node";
            port = 2222;
            jumpMember = "mac";
            remoteProgram = "/opt/nix%bin/nix-daemon";
          };
        mac = route;
      };
    };
    linux = {
      selfMember = "linux";
      allowedBuilders = ["workstation"];
      routes.workstation =
        route
        // {
          trust = {
            kind = "ca";
            publicKey = pin.publicKey;
            principal = "workstation.acme.example";
          };
        };
    };
    mac = {
      selfMember = "mac";
      allowedBuilders = ["mac" "builder"];
      lanes.builder = ["serial"];
      routes = {
        mac = route;
        builder =
          route
          // {
            hostname = "build-node";
            port = 2222;
          };
      };
    };
  };
  eval = {
    members ? {},
    controllers ? {},
    deployment ? {},
  }:
    (lib.evalModules {
      specialArgs.inputs.self.nixosConfigurations = {};
      modules = [
        ../../flake-modules/ansible.nix
        {
          options.flake = lib.mkOption {type = lib.types.lazyAttrsOf lib.types.raw;};
          config.mandala = {
            inherit deployment;
            extraMembers = members;
            build = {inherit controllers;};
          };
        }
      ];
    }).config;
  configured = eval {inherit members controllers;};
  fails = value: !(builtins.tryEval (builtins.deepSeq value true)).success;
in {inherit engine pin capacity members route controllers eval configured fails;}
