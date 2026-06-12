# Deploy-batch projection: the eval-once batch artifact per fan-out group.
# Per group, a linkFarm with one entry per member host pointing at its
# deploy-rs profile (the closure that embeds activate-rs), plus the
# deploy-rs deployChecks evaluated over ONLY that group's nodes — so
# `nix build .#deployBatch.<group>` is a single eval and a single build
# schedule, and copying the farm pushes the whole batch closure to a cache.
# Activation fans out through whatever orchestrates deploy-rs per host
# (ansible, in the reference pipeline).
#
# Group keys use the ansible-sanitized spelling (ansibleGroupsFor), so
# `.#deployBatch.<g>` and `ansible -l <g>` are the same name — one
# taxonomy, one spelling, on every fan-out surface. `all` is a synthetic
# meta-group spanning every deploy-rs node (no metadata facet covers a
# whole mixed fleet); right-hand side of `//` so it wins over any
# same-named facet group.
#
# pkgs and the deploy-rs lib arrive as ARGUMENTS (per-system, from the
# consumer's pins) — the engine carries no toolchain.
{
  lib,
  ansibleGroupsFor,
}: {
  # The consumer's per-system package set (linkFarm).
  pkgs,
  # deploy-rs.lib.<system> from the consumer's deploy-rs pin (deployChecks).
  deployLib,
  # The flake's deploy attrset — nodes plus any top-level deploy-rs
  # defaults; deployChecks sees it with nodes scoped per group.
  deploy,
  # name -> validated member; membership filters on deployRs.enable.
  hosts,
}: let
  deployable =
    lib.filterAttrs (_: host: host.deployment.deployRs.enable) hosts;

  allGroupKeys =
    lib.unique (lib.concatLists (lib.mapAttrsToList (_: ansibleGroupsFor) deployable));

  hostsInGroup = group:
    lib.attrNames (
      lib.filterAttrs (_name: h: lib.elem group (ansibleGroupsFor h)) deployable
    );

  groups =
    lib.genAttrs allGroupKeys hostsInGroup
    // {all = lib.attrNames deployable;};
in
  lib.mapAttrs (
    group: members:
      pkgs.linkFarm "deploy-batch-${group}" (
        map (h: {
          name = h;
          path = deploy.nodes.${h}.profiles.system.path;
        })
        members
        # deployChecks scoped to this group's nodes: schema validation +
        # per-profile activatability, without dragging the whole fleet's
        # checks into a small group's batch. Underscore prefix keeps the
        # check entries visually apart from host entries in the farm.
        ++ lib.mapAttrsToList (checkName: drv: {
          name = "_${checkName}";
          path = drv;
        })
        (deployLib.deployChecks
          (deploy
            // {
              nodes = lib.filterAttrs (n: _: lib.elem n members) deploy.nodes;
            }))
      )
  )
  groups
