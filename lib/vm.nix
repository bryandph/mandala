# Pure ownership contract. No configurations, artifacts or keys are forced.
{lib}: let
  require = message: condition: value:
    assert lib.assertMsg condition "mandala VM: ${message}"; value;
  sorted = xs: lib.sort builtins.lessThan (lib.unique xs);
  isVM = member: (member.kind or "standalone") == "vm";
  relation = member: member.vm or null;
  application = member: member.vmApplication or null;
  localValid = name: member:
    require "${name}: kind and vm relation disagree"
    ((isVM member) == (relation member != null))
    (require "${name} (owner ${
        if relation member == null
        then "missing"
        else member.vm.owner
      }): guest SSH enables and builder advertisements are forbidden"
      (!isVM member || (!member.deployment.deployRs.enable && !member.deployment.ansible.enable && (member.builder or null) == null))
      (require "${name}: application requires a standalone deploy-rs-managed NixOS owner with build metadata"
        (application member == null || (!isVM member && member.platform == "nixos" && member.deployment.deployRs.enable && member.build != null))
        true));
in rec {
  inherit isVM;
  hasVM = members: lib.any isVM (lib.attrValues members);

  # configurationNames is supplied by fleet assembly. Pure data consumers can
  # validate relations without loading (or evaluating) the configuration graph.
  validate = {
    members,
    configurations ? null,
    configurationNames ?
      if configurations == null
      then null
      else lib.attrNames configurations,
  }: let
    names = lib.attrNames members;
    guests = lib.filter (name: isVM members.${name}) names;
    owners = lib.filter (name: application members.${name} != null) names;
    guestsOf = owner: lib.filter (name: members.${name}.vm.owner == owner) guests;
    configured = name:
      (configurationNames == null || lib.elem name configurationNames)
      && (configurations == null || (configurations.${name}.config.system.build or {}) ? toplevel);
    guestValid = name: let
      guest = members.${name};
      ownerName = guest.vm.owner;
      owner = members.${ownerName};
      manifest = application owner;
      context = "guest ${name}, owner ${ownerName}";
    in
      require "${context}: owner is missing or self"
      (ownerName != name && members ? ${ownerName})
      (require "${context}: owner must be standalone, directly managed and NixOS; guest must be NixOS with build metadata"
        (!isVM owner && owner.platform == "nixos" && owner.deployment.deployRs.enable && owner.build != null && guest.platform == "nixos" && guest.build != null)
        (require "${context}: consumer NixOS configurations with toplevel artifacts are required"
          (configured name && configured ownerName)
          (require "${context}: owner application manifest is missing" (manifest != null)
            (require "${context}: unsupported backend, manifest version or system pair"
              (guest.vm.backend
                == "nixos-module"
                && manifest.backend == guest.vm.backend
                && manifest.manifestVersion == 1
                && lib.elem owner.build.system manifest.supportedSystems
                && lib.elem guest.build.system manifest.supportedSystems)
              (require "${context}: manifest guest/system does not match relation"
                (manifest.guests ? ${name} && manifest.guests.${name}.system == guest.build.system)
                true)))));
    ownerValid = name: let
      owner = members.${name};
      manifest = application owner;
    in
      require "owner ${name}: manifest must exactly match derived guest relations"
      (lib.attrNames manifest.guests == guestsOf name && guestsOf name != [])
      (require "owner ${name}: consumer configuration is missing" (configured name) true);
  in
    builtins.deepSeq (map (name: localValid name members.${name}) names)
    (builtins.deepSeq (map guestValid guests)
      (builtins.deepSeq (map ownerValid owners) members));

  project = members: let
    checked = validate {inherit members;};
    guests = lib.filter (name: isVM checked.${name}) (lib.attrNames checked);
    owners = sorted (map (name: checked.${name}.vm.owner) guests);
  in {
    guestsByOwner = lib.genAttrs owners (owner: lib.filter (name: checked.${name}.vm.owner == owner) guests);
    applications = lib.genAttrs owners (owner:
      checked.${owner}.vmApplication
      // {
        inherit owner;
        # A reference to the existing owner profile, never a second artifact
        # authority and never an evaluated derivation in this aggregate.
        application = ".#deploy.nodes.${owner}.profiles.system.path";
      });
  };

  # Inputs are already canonical member selectors. No effects or selector
  # re-evaluation: retain identities and expose conservative sibling impact.
  resolve = {
    members,
    selectedMembers,
    excludedMembers ? [],
  }: let
    checked = validate {inherit members;};
    names = lib.attrNames checked;
    selected = sorted selectedMembers;
    excluded = sorted excludedMembers;
    requestedGuests = lib.filter (name: isVM checked.${name}) selected;
    ownerOf = name:
      if isVM checked.${name}
      then checked.${name}.vm.owner
      else name;
    eligible = name: isVM checked.${name} || checked.${name}.deployment.deployRs.enable;
    units = sorted (map ownerOf (lib.filter eligible selected));
    index = (project checked).guestsByOwner;
    affected = owner: index.${owner} or [];
    conflicts =
      lib.filter (name: lib.elem name excluded)
      (sorted (units ++ lib.concatMap affected units));
  in
    require "selection names unknown members" (lib.all (name: lib.elem name names) (selected ++ excluded))
    (require "selected members conflict with exclusions" (lib.intersectLists selected excluded == [])
      (require "owner application conflicts with explicit exclusions: ${lib.concatStringsSep ", " conflicts}" (conflicts == []) {
        selectedMembers = selected;
        inherit requestedGuests;
        applicationUnits = lib.genAttrs units (owner:
          {
            inherit owner;
            requestedGuests = lib.filter (name: ownerOf name == owner) requestedGuests;
            affectedGuests = affected owner;
            potentiallyAffectedGuests = lib.subtractLists requestedGuests (affected owner);
            application = ".#deploy.nodes.${owner}.profiles.system.path";
          }
          // lib.optionalAttrs (index ? ${owner}) {
            inherit (checked.${owner}.vmApplication) backend manifestVersion;
          });
        skippedMembers = lib.filter (name: !eligible name) selected;
      }));
}
