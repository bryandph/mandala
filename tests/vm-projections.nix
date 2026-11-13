{lib}: let
  f = import ./fixtures/vm-members.nix {inherit lib;};
  inherit (f) engine members fails;
  agg = engine.aggregate {
    inherit members;
    projections.vm.guestsByOwner.forged = ["inventory"];
  };
  integrated = f.eval members;
  # Bypass schema validation deliberately: transport guards must stand alone.
  malformed = lib.mapAttrs (_: m:
    if m.kind != "vm"
    then m
    else
      lib.recursiveUpdate m {
        deployment.deployRs.enable = true;
        deployment.ansible.enable = true;
        builder.systems = [f.system];
      })
  members;
  inventory = engine.projections.ansibleInventory {
    hosts = malformed;
    extraHostvars = name:
      if lib.hasPrefix "guest" name
      then throw "hook reached a guest"
      else {hook = true;};
  };
  nodes = engine.projections.deployNodes {
    nixosConfigurations = f.configurations malformed;
    deploySettings = {};
    deploy-rs = throw "guard forced toolchain";
    nixpkgs = throw "guard forced nixpkgs";
  };
  batch = engine.projections.deployBatch {
    hosts = malformed;
    deploy.nodes = lib.genAttrs ["host-a" "host-c"] (_: {profiles.system.path = "/synthetic/owner-profile";});
    pkgs.linkFarm = name: entries: {inherit name entries;};
    deployLib.deployChecks = deploy: assert builtins.attrNames deploy.nodes == ["host-a" "host-c"] || builtins.attrNames deploy.nodes == ["host-a"] || builtins.attrNames deploy.nodes == ["host-c"]; {};
  };
  resolve = selection: engine.vm.resolve ({inherit members;} // selection);
  plan = resolve {selectedMembers = ["guest-b" "host-a" "guest-a" "guest-a" "inventory" "host-c"];};
  guestOnly = resolve {selectedMembers = ["guest-a"];};
  builderFixture = import ./fixtures/builders.nix {inherit lib;};
  builders = engine.projections.builders {
    members = builderFixture.members // {builder = builderFixture.members.builder // {kind = "vm";};};
    inherit (builderFixture) controllers;
  };
  capacityMembers =
    members
    // {
      host-a = engine.evalMember (f.ownerData // {builder = builderFixture.capacity;});
    };
  capacity = selfMember:
    engine.projections.builders {
      members = capacityMembers;
      controllers.acme = {
        inherit selfMember;
        allowedBuilders = ["host-a"];
        routes.host-a = builderFixture.route;
      };
    };
  noAdvertisement = engine.projections.builders {
    inherit members;
    controllers.acme = {
      selfMember = "host-c";
      allowedBuilders = [];
    };
  };
  guestJump = engine.projections.builders {
    members = capacityMembers;
    controllers.acme = {
      selfMember = "host-c";
      allowedBuilders = ["host-a"];
      routes = {
        host-a = builderFixture.route // {jumpMember = "guest-a";};
        guest-a = builderFixture.route;
      };
    };
  };
  legacy = import ./fixtures/vm-legacy.nix {inherit lib;};
  publication = agg.members.guest-a;
  emptyBatch = engine.projections.deployBatch {
    hosts = {guest-a = malformed.guest-a;};
    deploy = throw "guest batch touched deploy";
    pkgs.linkFarm = name: entries: {inherit name entries;};
    deployLib.deployChecks = _: {};
  };
in
  assert agg.schemaVersion == 2;
  assert builtins.attrNames agg.members == ["guest-a" "guest-b" "host-a" "host-c" "inventory"];
  assert agg.groups.guests_only == ["guest-a" "guest-b"];
  assert agg.groups.mixed == ["guest-a" "guest-b" "host-a"];
  assert agg.projections.vm.guestsByOwner == {host-a = ["guest-a" "guest-b"];};
  assert agg.projections.vm.applications.host-a.application == ".#deploy.nodes.host-a.profiles.system.path";
  assert agg.projections.vm.applications.host-a.owner == "host-a";
  assert agg.projections.vm.applications.host-a.guests.guest-b.system == "aarch64-linux";
  # JSON serialization cannot force owner or guest derivations. Configuration
  # toplevels throw in this fixture, including for controller-platform outputs.
  assert builtins.stringLength (builtins.toJSON integrated.flake.mandala) > 0;
  assert integrated.flake.mandala.schemaVersion == 2;
  assert builtins.attrNames integrated.flake.deploy.nodes == ["host-a" "host-c"];
  assert lib.all (platform: builtins.attrNames integrated.flake.deployByControllerPlatform.${platform}.nodes == ["host-a" "host-c"] && builtins.attrNames integrated.flake.mandala.projections.byControllerPlatform.${platform}.ansibleInventory.all.hosts == ["host-a" "host-c"]) ["darwin" "linux"];
  assert builtins.attrNames inventory.all.hosts == ["host-a" "host-c"];
  assert !(inventory.all.children ? guests_only);
  assert inventory.all.children.deploy_rs.hosts
  == {
    host-a = null;
    host-c = null;
  };
  assert builtins.attrNames nodes == ["host-a" "host-c"];
  assert !(batch ? guests_only);
  assert map (e: e.name) batch.all.entries == ["host-a" "host-c"];
  assert map (e: e.name) batch.mixed.entries == ["host-a"];
  assert emptyBatch.all.entries == [];
  assert fails builders;
  assert fails guestJump;
  assert (capacity "host-a").acme.buildMachines == [];
  assert builtins.length (capacity "host-c").acme.buildMachines == 2;
  assert lib.all (route: route.member == "host-a" && route.hostname == "host-a.acme.example" && route.strictHostKeyChecking && route.identitiesOnly && route.identityAgent == "none") (lib.attrValues (capacity "host-c").acme.ssh);
  assert noAdvertisement.acme.buildMachines == [];
  assert publication.name == "guest-a";
  assert publication.fqdn == "guest-a.acme.example";
  assert (lib.head publication.networks).address == "192.0.2.11";
  assert (lib.head publication.networks).assignment == "reservation";
  assert (lib.head publication.networks).roles == ["dns"];
  assert (lib.head publication.networks).mac == "02:00:00:00:00:11";
  assert plan.selectedMembers == ["guest-a" "guest-b" "host-a" "host-c" "inventory"];
  assert plan.requestedGuests == ["guest-a" "guest-b"];
  assert builtins.attrNames plan.applicationUnits == ["host-a" "host-c"];
  assert plan.skippedMembers == ["inventory"];
  assert guestOnly.applicationUnits.host-a.requestedGuests == ["guest-a"];
  assert guestOnly.applicationUnits.host-a.affectedGuests == ["guest-a" "guest-b"];
  assert guestOnly.applicationUnits.host-a.potentiallyAffectedGuests == ["guest-b"];
  assert fails (resolve {
    selectedMembers = ["guest-a"];
    excludedMembers = ["host-a"];
  });
  assert fails (resolve {
    selectedMembers = ["guest-a"];
    excludedMembers = ["guest-b"];
  });
  assert fails (resolve {
    selectedMembers = ["host-a"];
    excludedMembers = ["guest-b"];
  });
  assert fails (resolve {selectedMembers = ["missing"];});
  assert builtins.attrNames (resolve {selectedMembers = [];}).applicationUnits == [];
  assert legacy.aggregate.schemaVersion == 1;
  assert lib.all (name: builtins.toJSON legacy.${name} == builtins.readFile (./fixtures/vm-baseline + "/${name}.json")) ["aggregate" "deploy" "inventory" "batch"]; true
