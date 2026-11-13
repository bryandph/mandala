{lib}: let
  f = import ./fixtures/vm-members.nix {inherit lib;};
  inherit (f) engine members fails;
  validate = hosts:
    engine.vm.validate {
      members = hosts;
      configurationNames = lib.attrNames (f.configurations members);
    };
  guest = change: validate (members // {guest-a = lib.recursiveUpdate members.guest-a change;});
  owner = change: validate (members // {host-a = lib.recursiveUpdate members.host-a change;});
  evalFails = change: fails (engine.evalMember (lib.recursiveUpdate (f.guestData "guest-a") change));
  moduleFails = data: let
    result = lib.evalModules {
      modules = [
        ../schema/member.nix
        {
          options.assertions = lib.mkOption {
            type = lib.types.listOf lib.types.raw;
            default = [];
          };
          config.host = data;
        }
      ];
    };
  in
    lib.any (a: !a.assertion) result.config.assertions;
  missingConfiguration = engine.vm.validate {
    inherit members;
    configurationNames = ["host-a" "host-c" "guest-a"];
  };
  missingOwnerConfiguration = engine.vm.validate {
    inherit members;
    configurationNames = ["host-c" "guest-a" "guest-b"];
  };
  foreignManifest = owner {vmApplication.guests.inventory.system = f.system;};
  builder = {systems = [f.system];};
  inheritedEnable =
    (lib.evalModules {
      modules = [
        ../schema/member.nix
        {
          options.assertions = lib.mkOption {
            type = lib.types.listOf lib.types.raw;
            default = [];
          };
        }
        {host = f.guestData "guest-a";}
        {host.deployment.deployRs.enable = lib.mkDefault true;}
      ];
    }).config;
in
  assert builtins.deepSeq f.checked true;
  assert (engine.evalMember {
    name = "legacy-vm";
    category = "vm";
  }).kind
  == "standalone";
  assert members.guest-a.deployment.deployRs.enable == false;
  assert members.guest-a.deployment.ansible.enable == false;
  assert evalFails {vm = null;};
  assert evalFails {kind = "standalone";};
  assert evalFails {vm.backend = "guest-ssh";};
  assert evalFails {vm.owner = ["host-a" "host-c"];};
  assert evalFails {deployment.deployRs.enable = true;};
  assert evalFails {deployment.ansible.enable = true;};
  assert evalFails {inherit builder;};
  assert moduleFails ((f.guestData "guest-a") // {kind = "standalone";});
  assert lib.any (a: !a.assertion) inheritedEnable.assertions;
  assert fails (guest {vm.owner = "missing";});
  assert fails (guest {vm.owner = "guest-a";});
  assert fails (guest {vm.owner = "guest-b";});
  assert fails (guest {vm.owner = "inventory";});
  assert fails (validate (members
    // {
      guest-a = members.guest-a // {vm = members.guest-a.vm // {owner = "guest-b";};};
      guest-b = members.guest-b // {vm = members.guest-b.vm // {owner = "guest-a";};};
    }));
  assert fails (owner {deployment.deployRs.enable = false;});
  assert fails (owner {platform = "darwin";});
  assert fails (owner {vmApplication = null;});
  assert fails (owner {vmApplication.backend = "other";});
  assert fails (owner {vmApplication.manifestVersion = 2;});
  assert fails (owner {vmApplication.supportedSystems = [f.system];});
  assert fails (owner {vmApplication.supportedSystems = ["aarch64-linux"];});
  assert fails (guest {build = null;});
  assert fails (guest {platform = "darwin";});
  assert fails missingConfiguration;
  assert fails missingOwnerConfiguration;
  assert fails (engine.vm.validate {
    inherit members;
    configurations = (f.configurations members) // {guest-a.config = {};};
  });
  assert fails (engine.vm.validate {
    inherit members;
    configurations = (f.configurations members) // {host-a.config = {};};
  });
  assert fails foreignManifest;
  assert fails (owner {vmApplication.guests.guest-a.system = "aarch64-linux";});
  assert fails (validate (members // {host-a = members.host-a // {vmApplication = members.host-a.vmApplication // {guests = {};};};}));
  assert fails (engine.aggregate {members = members // {guest-a = members.guest-a // {deployment = lib.recursiveUpdate members.guest-a.deployment {ansible.enable = true;};};};});
  # The integrated module cannot accept absent configurations via extraMembers.
  assert fails ((lib.evalModules {
    specialArgs.inputs.self.nixosConfigurations = {};
    modules = [
      ../flake-modules/fleet.nix
      {
        options.flake = lib.mkOption {type = lib.types.lazyAttrsOf lib.types.raw;};
        config.mandala.extraMembers = members;
      }
    ];
  }).config.flake.mandala); true
