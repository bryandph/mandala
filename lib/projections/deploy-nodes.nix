# deploy-rs nodes projection: built nixosConfigurations in, `deploy.nodes`
# attrset out. The toolchains arrive as ARGUMENTS — deploy-rs and nixpkgs
# are the CONSUMER's pinned flakes; the engine's flake.lock never carries
# them.
#
# Membership: every configuration whose member enables
# deployment.deployRs.enable (the schema's class-agnostic opt-in).
#
# Cross-compile scars ship here, keyed off contract data
# (host.build.buildPlatform), because cross-built fleets are a target
# audience and the fixes are metadata-driven:
#   - deploy-rs lib selection: a host system outside deploy-rs's supported
#     set (e.g. armv7l-linux) falls back to the member's declared
#     buildPlatform for schema/check machinery; the activate closure still
#     uses the host-platform pkgs.
#   - shebang rewrite: deploy-rs's activate.* scripts are written with
#     build-platform shebangs; on a cross-built host the kernel can't exec
#     build-platform bash ("Exec format error" at activation). The activate
#     profile is post-processed to host-platform shebangs — two tiny
#     scripts, cheap.
{lib}: {
  # The consumer's deploy-rs flake (.overlays.default + .lib.<system>).
  deploy-rs,
  # The consumer's nixpkgs flake (imported per node to apply the overlay).
  nixpkgs,
  # Built configurations; members are read from .config.host.
  nixosConfigurations,
  # name -> settings already flattened by the fleet module's single merge.
  deploySettings,
}: let
  deployable =
    lib.filterAttrs
    (_: cfg: cfg.config.host.deployment.deployRs.enable)
    nixosConfigurations;

  # Systems deploy-rs ships lib machinery for — the fallback decision is
  # made against the actual toolchain, not a hardcoded list.
  supportedSystems = lib.attrNames deploy-rs.lib;

  mkNode = name: cfg: let
    host = cfg.config.host;
    settings = assert lib.assertMsg (deploySettings ? ${name})
    "mandala deploy-nodes: ${name}: flattened deploy settings are missing";
      deploySettings.${name};
    nodeSettings = removeAttrs settings ["activation"];
    hostSystem = cfg.pkgs.stdenv.hostPlatform.system;
    cross =
      host.build
      != null
      && host.build.buildPlatform != null
      && host.build.buildPlatform != host.build.system;
    system =
      if lib.elem hostSystem supportedSystems
      then hostSystem
      else
        assert lib.assertMsg cross
        "mandala deploy-nodes: ${name}: host system ${hostSystem} is outside deploy-rs's supported set and the member declares no build.buildPlatform to fall back to";
          host.build.buildPlatform;

    deployPkgs = import nixpkgs {
      inherit system;
      overlays = [
        deploy-rs.overlays.default
        (_: super: {
          deploy-rs = {
            # cfg.pkgs.deploy-rs so cross-compiled hosts get a
            # host-platform activate binary. For non-cross hosts this is
            # identical to pkgs.deploy-rs.
            deploy-rs = cfg.pkgs.deploy-rs;
            inherit (super.deploy-rs) lib;
          };
        })
      ];
    };

    rawActivate =
      if settings.activation == "boot"
      then deployPkgs.deploy-rs.lib.activate.custom cfg.config.system.build.toplevel "./bin/switch-to-configuration boot"
      else deployPkgs.deploy-rs.lib.activate.nixos cfg;
    activate =
      if !cross
      then rawActivate
      else
        cfg.pkgs.runCommand "${rawActivate.name}-cross-shebang" {} ''
          cp -r --no-preserve=mode ${rawActivate} $out
          for f in $out/activate-rs $out/deploy-rs-activate; do
            [ -f "$f" ] || continue
            sed -i "1c#!${cfg.pkgs.runtimeShell}" "$f"
          done
        '';
  in
    nodeSettings
    // {
      profiles.system.path = activate;
    };
in
  lib.mapAttrs mkNode deployable
