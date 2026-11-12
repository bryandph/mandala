{lib}: let
  f = import ./fixtures/builders.nix {inherit lib;};
  p = f.configured.flake.mandala.projections.builders.byController;
  wsl = p.wsl;
  targets = lib.filter (r: r.lane != null) (lib.attrValues wsl.ssh);
  builder = lib.filter (r: r.member == "builder") targets;
  builderMachines = lib.take 2 wsl.buildMachines;
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
      controllers = f.controllers // {wsl = f.controllers.wsl // {allowedBuilders = ["builder" "workstation"];};};
    }).flake.mandala.projections.builders.byController;
  ca = {
    kind = "ca";
    publicKey = f.pin.publicKey;
  };
  project = members: route:
    f.engine.builders.project {
      inherit members;
      controllers.acme = {
        selfMember = "linux";
        allowedBuilders = ["workstation"];
        routes.workstation = f.route // {trust = ca;} // route;
      };
    };
  shortCA = (project f.members {hostname = "build-short";}).acme;
  inheritedCA =
    (project (f.members
      // {
        workstation =
          f.members.workstation
          // {
            builder =
              f.members.workstation.builder
              // {
                hostname = "member-short";
                trust = ca // {principal = null;};
              };
          };
      }) {trust = null;}).acme;
  fqdnCA = (project f.members {}).acme;
  explicitCA =
    (project f.members {
      hostname = "build-short";
      trust = ca // {principal = "cert.acme.example";};
    }).acme;
  jumpCA =
    (f.engine.builders.project {
      inherit (f) members;
      controllers.acme =
        f.controllers.wsl
        // {
          routes =
            f.controllers.wsl.routes
            // {
              mac =
                f.route
                // {
                  hostname = "bastion-short";
                  trust = ca;
                  port = 2200;
                };
              builder = f.controllers.wsl.routes.builder // {trust = ca;};
            };
        };
    }).acme;
  missingName = project (f.members // {workstation = removeAttrs f.members.workstation ["fqdn"];}) {};
  syntheticPin64 = "c3NoLWVkMjU1MTkgQUFBQUMzTnphQzFsWkRJMU5URTVBQUFBSUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE=";
in
  assert lib.all (r: r.trust.principal == "build-short" && r.hostKeyAlias == "build-short") (lib.attrValues shortCA.ssh);
  assert lib.all (h: h.hostNames == ["build-short"] && h.certAuthority) (lib.attrValues shortCA.knownHosts);
  assert lib.all (r: r.trust.principal == "member-short") (lib.attrValues inheritedCA.ssh);
  assert lib.all (r: r.trust.principal == f.members.workstation.fqdn) (lib.attrValues fqdnCA.ssh);
  assert f.fails missingName;
  assert lib.all (r: r.hostKeyAlias == "cert.acme.example" && r.trust.principal == "cert.acme.example") (lib.attrValues explicitCA.ssh);
  assert lib.all (r:
    r.hostKeyAlias
    == (
      if r.member == "mac"
      then "bastion-short"
      else "build-node"
    )) (lib.filter (r: r.member != "workstation") (lib.attrValues jumpCA.ssh));
  assert lib.all (h:
    h.hostNames
    == [
      (
        if h.member == "mac"
        then "[bastion-short]:2200"
        else "[build-node]:2222"
      )
    ]) (lib.filter (h: h.member != "workstation") (lib.attrValues jumpCA.knownHosts));
  assert lib.all (r: r.hostKeyAlias != wsl.ssh.${r.jumpAlias}.hostKeyAlias) builder;
  assert builtins.length wsl.buildMachines == 3;
  assert builtins.length p.mac.buildMachines == 1;
  assert builtins.length p.linux.buildMachines == 1;
  assert builtins.length builder == 2;
  assert lib.all (r: r.port == 2222 && r.hostname == "build-node" && r.jumpAlias != null) builder;
  assert (lib.head builderMachines).maxJobs == 1 && (lib.last builderMachines).maxJobs == 8;
  assert lib.all (m: lib.hasSuffix "?remote-program=%2Fopt%2Fnix%25bin%2Fnix-daemon" m.hostName) builderMachines;
  assert lib.all (m: !(m ? remoteProgram) && m.protocol == "ssh-ng" && m.sshKey == "/keys/operator-alice") wsl.buildMachines;
  assert lib.all (r: r.strictHostKeyChecking && r.identitiesOnly && r.identityAgent == "none") (lib.attrValues wsl.ssh);
  assert lib.all (r: r.jumpAlias == null || wsl.ssh ? ${r.jumpAlias}) (lib.attrValues wsl.ssh);
  assert lib.all (r: r.member != "mac") (lib.attrValues p.mac.ssh);
  assert lib.all (r: r.jumpAlias == null && r.member == "builder") (lib.attrValues p.mac.ssh);
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
