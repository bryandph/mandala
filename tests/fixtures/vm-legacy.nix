# Standalone transport byte-parity fixture, captured before VM support.
{lib}: let
  engine = import ../../lib {inherit lib;};
  managed = import ./builder-managed-legacy.nix {inherit lib;};
  member = engine.evalMember (lib.recursiveUpdate (import ../../examples/fake-fleet/member.nix) {
    deployment.deployRs.enable = true;
    deployment.ansible.enable = true;
  });
  batch = engine.projections.deployBatch {
    hosts.${member.name} = member;
    deploy = managed.deploy;
    pkgs.linkFarm = name: entries: {inherit name entries;};
    deployLib.deployChecks = _: {schema = "/synthetic/schema-check";};
  };
in
  managed // {inherit batch;}
