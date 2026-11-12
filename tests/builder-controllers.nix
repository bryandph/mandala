{lib}: let
  f = import ./fixtures/builders.nix {inherit lib;};
  inherit (f) fails;
  profile = f.controllers.wsl;
  eval = p:
    (f.eval {
      inherit (f) members;
      controllers.acme = p;
    }).flake.mandala;
  invalid = patch: fails (eval (lib.recursiveUpdate profile patch));
  routeBad = patch: invalid {routes.builder = patch;};
in
  assert builtins.deepSeq f.configured.flake.mandala true;
  assert lib.all invalid [
    {selfMember = "missing";}
    {allowedBuilders = ["missing"];}
    {allowedBuilders = ["wsl"];}
    {allowedBuilders = ["builder" "builder"];}
    {lanes.mac = ["wide"];}
    {lanes.builder = ["unknown"];}
    {lanes.builder = ["wide" "wide"];}
    {routes.unknown = f.route;}
    {routes.builder.jumpMember = "unknown";}
    {routes.builder.jumpMember = "builder";}
    {routes.mac.jumpMember = "builder";}
    {routes.workstation.jumpMember = "missing";}
    {routes.mac.identityFile = null;}
  ];
  assert lib.all routeBad [
    {identityFile = ./fixtures;}
    {identityFile = "${./fixtures}";}
    {identityFile = "relative";}
    {identityFile = "~/.ssh/key";}
    {identityFile = "/keys/$HOME";}
    {identityFile = "/keys/a\nProxyCommand bad";}
    {identityFile = "/keys/a b";}
    {hostname = "host;bad";}
    {hostname = "host?remote-program=bad";}
    {sshUser = "builder@host";}
    {port = 0;}
    {protocol = "ssh";}
    {remoteProgram = "relative";}
    {remoteProgram = "/bin/nix?bad";}
    {trust = {};}
    {
      trust = {
        kind = "pin";
        publicKey = "ssh-ed25519 AA\nbad";
      };
    }
    {strictHostKeyChecking = false;}
  ];
  assert fails (eval (removeAttrs profile ["selfMember"]));
  assert fails (eval (removeAttrs profile ["allowedBuilders"]));
  assert fails (eval (profile // {routes = removeAttrs profile.routes ["builder"];}));
  assert fails ((f.eval {
    inherit (f) members;
    controllers."unsafe name" = profile;
  }).flake.mandala);
  # Missing final/jump credentials, including on an otherwise unused route.
  assert fails ((f.eval {
    members =
      f.members
      // {
        builder = f.engine.evalMember {
          name = "builder";
          builder = {systems = ["x86_64-linux"];};
        };
      };
    controllers.acme = profile;
  }).flake.mandala);
  assert fails (eval (profile // {routes = profile.routes // {linux = f.route;};}));
  assert fails ((f.eval {
    members =
      f.members
      // {
        workstation = f.engine.evalMember {
          name = "workstation";
          domain = "unsafe\nProxyCommand bad";
          builder = f.capacity;
        };
      };
    controllers.acme = profile;
  }).flake.mandala);
  assert (eval (profile // {lanes.builder = [];})).projections.builders.byController.acme.buildMachines != [];
  assert (eval (profile
    // {
      allowedBuilders = [];
      lanes = {};
    })).projections.builders.byController.acme.buildMachines
  == [];
  assert (eval (profile // {allowedBuilders = [];})).projections.builders.byController.acme
  == {
    buildMachines = [];
    ssh = {};
    knownHosts = {};
  };
  assert builtins.deepSeq ((f.eval {
    members =
      f.members
      // {
        mac = f.engine.evalMember {
          name = "mac";
          platform = "darwin";
        };
      };
    controllers.acme = lib.recursiveUpdate profile {
      routes.mac =
        f.route
        // {
          hostname = "jump.acme.example";
          sshUser = "jump";
          trust = f.pin;
        };
    };
  }).flake.mandala)
  true; true
