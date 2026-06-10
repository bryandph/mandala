# Engine library. Pure nixpkgs.lib — no packages, no operator data.
{lib}: rec {
  # Schema modules (paths, importable into any module evaluation).
  schemas = {
    operator = ../schema/operator.nix;
    topology = ../schema/topology.nix;
    pki = ../schema/pki.nix;
    member = ../schema/member.nix;
  };

  # Evaluate operator data against the schema; returns the validated
  # operator attrset including derived fields. Invalid data fails the
  # consumer's eval, not a later deploy.
  evalOperator = data:
    (lib.evalModules {
      modules = [
        schemas.operator
        {operator = data;}
      ];
    }).config.operator;

  # Same contract for topology data (`{vlans = {...};}`).
  evalTopology = data:
    (lib.evalModules {
      modules = [
        schemas.topology
        {topology = data;}
      ];
    }).config.topology;

  # Same contract for PKI trust anchors (`{cas = {...};}`).
  evalPki = data:
    (lib.evalModules {
      modules = [
        schemas.pki
        {pki = data;}
      ];
    }).config.pki;

  # Evaluate one member's data against the member schema, plus the
  # cross-field invariants the module system can't express per-option
  # (NixOS consumers enforce the same invariants as host assertions).
  evalMember = data: let
    m =
      (lib.evalModules {
        modules = [
          schemas.member
          {host = data;}
        ];
      }).config.host;
    withRole = role: lib.filter (n: lib.elem role n.roles) m.networks;
  in
    assert lib.assertMsg
    (lib.all (role: lib.length (withRole role) <= 1) ["dns" "reach" "gateway" "management"])
    "member ${m.name}: at most one network may carry each address role";
    assert lib.assertMsg
    (lib.all (n: n.assignment != "reservation" || n.address != null) m.networks)
    "member ${m.name}: assignment = \"reservation\" requires an address (it IS the reservation)"; m;

  # The one group taxonomy behind every authority — deploy-rs `@group`,
  # ansible `-l group`, and sops recipient groups all call this, so they
  # cannot drift. Takes a validated member/host attrset (a NixOS host's
  # `config.host` or an evalMember result).
  groupsFor = host:
    lib.unique (
      [host.platform host.architecture host.category]
      ++ lib.optional (host.role != null) host.role
      ++ host.tags
      ++ host.deployment.ansible.groups
    );
}
