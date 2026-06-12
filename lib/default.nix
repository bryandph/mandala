# Engine library. Pure nixpkgs.lib — no packages, no operator data.
{lib}: rec {
  # Schema modules (paths, importable into any module evaluation).
  schemas = {
    operator = ../schema/operator.nix;
    topology = ../schema/topology.nix;
    pki = ../schema/pki.nix;
    member = ../schema/member.nix;
    mesh = ../schema/mesh.nix;
    secrets = ../schema/secrets.nix;
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

  # Same contract for the overlay-mesh table (`{members = {...};}`).
  evalMesh = data:
    (lib.evalModules {
      modules = [
        schemas.mesh
        {mesh = data;}
      ];
    }).config.mesh;

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

  # Evaluate secret declarations against the schema AND the fleet: readers
  # resolve to member names (explicit members ∪ group members ∪ the
  # sops-identity set for `all`), and the cross-field invariants fail eval
  # by name instead of silently mis-sealing a file. Returns the validated
  # declarations, each augmented with `resolvedReaders` (sorted member
  # names) for the projection layer.
  evalSecrets = {
    # Plain declarations data (`{<name> = {path, readers, custody};}`).
    declarations,
    # name -> validated member, the fleet the readers resolve against.
    hosts,
  }: let
    secrets =
      (lib.evalModules {
        modules = [
          schemas.secrets
          {secrets = declarations;}
        ];
      }).config.secrets;

    memberNames = lib.attrNames hosts;
    sopsMembers =
      lib.filter (n: hosts.${n}.deployment.sops.recipient != null) memberNames;
    resolveGroup = g:
      lib.filter (n: lib.elem g (groupsFor hosts.${n})) memberNames;

    resolvedReadersOf = s:
      lib.sort (a: b: a < b) (lib.unique (
        s.readers.members
        ++ lib.concatMap resolveGroup s.readers.groups
        ++ lib.optionals s.readers.all sopsMembers
      ));

    # Offender lists, one per invariant — each assert names every violation.
    forSecrets = f: lib.concatLists (lib.mapAttrsToList f secrets);

    dupPaths = let
      paths = lib.mapAttrsToList (_: s: s.path) secrets;
    in
      lib.attrNames (lib.filterAttrs (_: c: c > 1) (lib.foldl'
        (acc: p: acc // {${p} = (acc.${p} or 0) + 1;})
        {}
        paths));

    adminOnlyConflicts = forSecrets (name: s:
      lib.optional
      (s.readers.adminOnly && (s.readers.members != [] || s.readers.groups != [] || s.readers.all))
      "secret '${name}' is adminOnly but declares other readers");

    unknownMembers = forSecrets (name: s:
      map (n: "secret '${name}' names unknown member '${n}'")
      (lib.filter (n: !(hosts ? ${n})) s.readers.members));

    emptyGroups = forSecrets (name: s:
      map (g: "secret '${name}' references group '${g}', which resolves to no members")
      (lib.filter (g: resolveGroup g == []) s.readers.groups));

    missingRecipients = forSecrets (name: s:
      map (n: "secret '${name}' resolves to reader '${n}', which has no sops recipient")
      (lib.filter (n: hosts.${n}.deployment.sops.recipient == null)
        (resolvedReadersOf s)));

    fail = msgs: "mandala evalSecrets:\n  " + lib.concatStringsSep "\n  " msgs;
  in
    assert lib.assertMsg (dupPaths == [])
    (fail (map (p: "path '${p}' is declared by more than one secret") dupPaths));
    assert lib.assertMsg (adminOnlyConflicts == []) (fail adminOnlyConflicts);
    assert lib.assertMsg (unknownMembers == []) (fail unknownMembers);
    assert lib.assertMsg (emptyGroups == []) (fail emptyGroups);
    assert lib.assertMsg (missingRecipients == []) (fail missingRecipients);
      lib.mapAttrs (_: s: s // {resolvedReaders = resolvedReadersOf s;}) secrets;

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

  # ansible group names may only contain [A-Za-z0-9_]; map every other
  # character to `_` (edge-compute -> edge_compute). Tags stay kebab-case as
  # the human-facing source of truth — only fan-out surface keys (ansible
  # inventory groups, deploy batch keys) are sanitized. Canonical here for
  # the same reason groupsFor is: a per-consumer copy is a drift vector.
  sanitizeGroupName = name:
    lib.stringAsChars (c:
      if lib.match "[A-Za-z0-9_]" c == null
      then "_"
      else c)
    name;

  # groupsFor in the ansible-safe spelling — the one spelling shared by
  # every fan-out surface (`ansible -l <group>`, `deployBatch.<group>`).
  # unique again: sanitization can merge two raw names into one key.
  ansibleGroupsFor = host: lib.unique (map sanitizeGroupName (groupsFor host));

  # Projections: pure functions from validated fleet data to tool-shaped
  # output. Toolchains (pkgs, deploy-rs, …) are injected as ARGUMENTS by
  # the caller — the engine pins none of them.
  projections = {
    ansibleInventory = import ./projections/ansible-inventory.nix {inherit lib ansibleGroupsFor;};
    sopsConfig = import ./projections/sops-config.nix {inherit lib;};
  };

  # nixos-facter report predicates (pattern: nixpkgs
  # nixos/modules/hardware/facter/lib.nix). Reports corroborate authored
  # member data — they never set or override it. Consumers gate on a
  # non-empty report and cross-check claims at eval time.
  facter = {
    # The system the report was captured on ("x86_64-linux"), or null when
    # the report (or field) is absent.
    systemOf = report: report.system or null;

    # Every interface name the report observed — flattened
    # hardware.network_interface[].unix_device_names. Includes virtual
    # netdevs (VLANs, bridges) that existed at capture time.
    interfaceNamesOf = report:
      lib.unique (
        lib.concatMap (i: i.unix_device_names or [])
        (report.hardware.network_interface or [])
      );
  };
}
