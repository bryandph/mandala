{lib}: let
  f = import ./fixtures/builders.nix {inherit lib;};
  p = f.configured.flake.mandala.projections.builders.byController;
  wsl = p.wsl;
  targets = lib.filter (r: r.lane != null) (lib.attrValues wsl.ssh);
  vm = lib.filter (r: r.member == "vm") targets;
  vmMachines = lib.take 2 wsl.buildMachines;
  permutedMembers = lib.mapAttrs (_: m:
    if m.builder == null
    then m
    else
      m
      // {
        builder =
          m.builder
          // {
            systems = lib.reverseList m.builder.systems;
            supportedFeatures = lib.reverseList m.builder.supportedFeatures;
          };
      })
  f.members;
  permuted =
    (f.eval {
      members = permutedMembers;
      controllers = f.controllers // {wsl = f.controllers.wsl // {allowedBuilders = ["vm" "workstation"];};};
    }).flake.mandala.projections.builders.byController;
  syntheticPin64 = "c3NoLWVkMjU1MTkgQUFBQUMzTnphQzFsWkRJMU5URTVBQUFBSUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE=";
in
  assert builtins.length wsl.buildMachines == 3;
  assert builtins.length p.mac.buildMachines == 1;
  assert builtins.length p.linux.buildMachines == 1;
  assert builtins.length vm == 2;
  assert lib.all (r: r.port == 2222 && r.hostname == "localhost" && r.jumpAlias != null) vm;
  assert (lib.head vmMachines).maxJobs == 1 && (lib.last vmMachines).maxJobs == 8;
  assert lib.all (m: lib.hasSuffix "?remote-program=%2Fopt%2Fnix%25bin%2Fnix-daemon" m.hostName) vmMachines;
  assert lib.all (m: !(m ? remoteProgram) && m.protocol == "ssh-ng" && m.sshKey == "/keys/operator-alice") wsl.buildMachines;
  assert lib.all (r: r.strictHostKeyChecking && r.identitiesOnly && r.identityAgent == "none") (lib.attrValues wsl.ssh);
  assert lib.all (r: r.jumpAlias == null || wsl.ssh ? ${r.jumpAlias}) (lib.attrValues wsl.ssh);
  assert lib.all (r: r.member != "mac") (lib.attrValues p.mac.ssh);
  assert lib.all (r: r.jumpAlias == null && r.member == "vm") (lib.attrValues p.mac.ssh);
  assert lib.all (r: lib.head r.hostNames == "[${r.hostKeyAlias}]:2222") (lib.attrValues p.mac.knownHosts);
  assert (lib.head p.linux.buildMachines).publicHostKey == null;
  assert lib.all (r: r.certAuthority && r.principal == "workstation.acme.example" && r.matchName == r.principal) (lib.attrValues p.linux.knownHosts);
  assert f.engine.builders.base64 "ssh-ed25519 AAAA" == "c3NoLWVkMjU1MTkgQUFBQQ==";
  assert (lib.head wsl.buildMachines).publicHostKey == syntheticPin64;
  assert builtins.toJSON permuted == builtins.toJSON p;
  assert f.configured.flake.buildMachinesByController == lib.mapAttrs (_: x: x.buildMachines) p;
  assert builtins.fromJSON (builtins.toJSON p) == p;
  assert f.engine.projections.builders {inherit (f) members controllers;} == p;
  assert lib.all (name: builtins.stringLength name <= 63) (lib.concatMap (c: lib.attrNames c.ssh) (lib.attrValues p));
  assert builtins.length (lib.unique (lib.concatMap (c: lib.attrNames c.ssh) (lib.attrValues p))) == 6;
  assert lib.all (r: r.hostKeyAlias == r.trust.principal) (lib.attrValues p.linux.ssh); true
