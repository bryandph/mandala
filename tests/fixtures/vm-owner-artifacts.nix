# Consumer-owned authoring example. Guests are independent configurations;
# only the owner imports their already-defined immutable outputs.
{lib}: let
  system = "x86_64-linux";
  guestNames = ["guest-a" "guest-b"];
  base = name: {
    boot.isContainer = true;
    networking.hostName = name;
    system.stateVersion = "25.11";
    host = {
      inherit name;
      build = {inherit system;};
    };
  };
  guests = lib.genAttrs guestNames (name:
    lib.nixosSystem {
      inherit system;
      modules = [
        ../../schema/member.nix
        (base name)
        {
          host = {
            kind = "vm";
            vm = {
              owner = "host-a";
              backend = "nixos-module";
            };
            deployment.deployRs.enable = false;
            deployment.ansible.enable = false;
          };
        }
      ];
    });
  # This fictional adapter is intentionally not a qualified runtime. Nothing
  # executes it, boots these configurations, or deploys an owner in this check.
  consumerModule = {
    config,
    pkgs,
    ...
  }: let
    adapter = pkgs.writeShellScript "fixture-runtime-adapter" "exit 1";
    contract = pkgs.writeText "fixture-runtime-contract" "fixture-only";
    manifest = {
      manifestVersion = 1;
      owner = config.host.name;
      backend = "nixos-module";
      ownerSystem = system;
      # Filled from the actual build output, not a recursive Nix reference to
      # config.system.build.toplevel inside its own manifest derivation.
      ownerGeneration = "@OWNER_GENERATION@";
      supportedSystems = [system];
      adapter = toString adapter;
      guests = lib.genAttrs guestNames (name: {
        inherit system;
        desiredIdentity = toString guests.${name}.config.system.build.toplevel;
        artifact = toString guests.${name}.config.system.build.toplevel;
        runtimeInstance = "fixture-${name}";
        readiness = {
          contract = toString contract;
          deadlineSeconds = 30;
        };
        recoveryContract = toString contract;
      });
    };
    template = pkgs.writeText "fixture-vm-application.json.in" (builtins.toJSON manifest);
  in {
    host = {
      deployment.deployRs.enable = true;
      vmApplication = {
        backend = manifest.backend;
        manifestVersion = manifest.manifestVersion;
        supportedSystems = manifest.supportedSystems;
        guests = lib.genAttrs guestNames (_: {inherit system;});
      };
    };
    system.systemBuilderCommands = ''
      mkdir -p "$out/mandala"
      substitute ${template} "$out/mandala/vm-application.json" \
        --replace-fail '@OWNER_GENERATION@' "$out"
    '';
    system.build.fixtureVmManifest = manifest;
  };
  owner = lib.nixosSystem {
    inherit system;
    modules = [../../schema/member.nix (base "host-a") consumerModule];
  };
  manifest = owner.config.system.build.fixtureVmManifest;
  compatible = value:
    lib.attrNames value.guests
    == lib.attrNames owner.config.host.vmApplication.guests
    && lib.all (name: value.guests.${name}.system == owner.config.host.vmApplication.guests.${name}.system) (lib.attrNames value.guests);
in {
  inherit owner guests manifest compatible;
  unrelated = manifest // {guests = manifest.guests // {unrelated = manifest.guests.guest-a;};};
}
