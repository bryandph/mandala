# mandala showcase — a complete fictional fleet exercising every
# projection. This is the "see the whole product at once" example;
# examples/fake-fleet stays the minimal operator-value-free check
# fixture. Unlike the engine itself, examples MAY pin third-party flakes
# (deploy-rs, terranix) — that's the layering rule, not an exception.
#
# Assert vs illustrate (see README.md): the data projections (aggregate,
# ansible inventory, sops config, deploy node/batch shapes, the terranix
# render) are ASSERTED by checks.showcase; actually deploying, applying
# terraform, or running ansible against these fictional members is only
# ILLUSTRATED — there is nothing real to push to.
{
  description = "mandala showcase — a complete fictional fleet";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-26.05";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    mandala = {
      url = "github:bryandph/mandala";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Toolchains are the CONSUMER's pins — the engine never carries them.
    deploy-rs = {
      url = "github:serokell/deploy-rs";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    terranix = {
      url = "github:terranix/terranix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs @ {
    nixpkgs,
    flake-parts,
    mandala,
    ...
  }:
    flake-parts.lib.mkFlake {inherit inputs;} ({config, ...}: let
      operator = mandala.lib.evalOperator (import ./operator.nix);
      topology = mandala.lib.evalTopology (import ./topology.nix);
      mesh = mandala.lib.evalMesh (import ./mesh.nix);
    in {
      # Darwin included deliberately: the checks are pure-eval asserts over
      # projection data, valid from any system (members stay linux).
      systems = ["x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin"];

      imports = [
        mandala.flakeModules.fleet
        mandala.flakeModules.ansible
        mandala.flakeModules.sops
        mandala.flakeModules.deploy
      ];

      mandala = {
        inherit operator;

        # Facts-only members: validated plain data, no management surface.
        extraMembers = {
          router = mandala.lib.evalMember (import ./members/router.nix);
        };

        # Consumer hook: repo-specific hostvars merge after the engine
        # defaults (and could override them).
        ansible.extraHostvars = name: {showcase_dir = "hosts/${name}";};

        sops = {
          operatorAnchor = operator.gpg.fingerprint;
          declarations = import ./secrets.nix;
        };
      };

      # NixOS members author host.* IN-CONFIG — the configuration IS the
      # inventory entry. A real fleet wraps this in a factory.
      flake.nixosConfigurations = {
        web = nixpkgs.lib.nixosSystem {
          system = "x86_64-linux";
          modules = [mandala.lib.schemas.member ./hosts/web.nix];
        };
        cache = nixpkgs.lib.nixosSystem {
          system = "aarch64-linux";
          modules = [mandala.lib.schemas.member ./hosts/cache.nix];
        };
      };

      perSystem = {
        pkgs,
        system,
        ...
      }: {
        # The terranix pattern: terraform consumes the contract DIRECTLY
        # in terranix modules (no intermediate files) — here, DNS records
        # rendered from members' dns-role addresses and the mesh table.
        packages.network-tf = inputs.terranix.lib.terranixConfiguration {
          inherit system;
          modules = [./terranix/dns.nix];
          extraArgs = {
            fleet = config.flake.mandala;
            inherit mesh topology;
          };
        };

        # ASSERTED: every projection evaluates and has the expected shape.
        checks.showcase = let
          agg = config.flake.mandala;
          inventory = config.flake.ansibleInventory;
          sops = config.flake.sopsConfig;
          nodes = config.flake.deploy.nodes;
          lib = nixpkgs.lib;
        in
          assert agg.schemaVersion == 1;
          assert builtins.attrNames agg.members == ["cache" "router" "web"];
          assert agg.groups.web == ["web"]; # role group
          
          assert agg.groups.gateway == ["router"]; # facts-only member in the taxonomy
          
          assert agg.operator.gpg.keyIdShort != "";
          # ansible inventory: members + hook var + guard group
          assert builtins.attrNames inventory.all.hosts == ["cache" "web"]; # router is facts-only
          
          assert inventory.all.hosts.web.showcase_dir == "hosts/web";
          assert inventory.all.hosts.web.ansible_python_interpreter == "/run/current-system/sw/bin/python3";
          assert inventory.all.children.deploy_rs.hosts
          == {
            cache = null;
            web = null;
          };
          # sops: keys = anchor + one recipient per keyed member; admin rule pgp-only
          assert lib.length sops.keys == 3;
          assert !(lib.head (lib.head sops.creation_rules).key_groups ? age);
          # deploy: nodes + batch keys (eval-only — nothing is built)
          assert builtins.attrNames nodes == ["cache" "web"];
          assert nodes.web.hostname == "web.fleet.example";
          assert lib.elem "cache" (builtins.attrNames inputs.self.legacyPackages.${system}.deployBatch);
          assert lib.elem "all" (builtins.attrNames inputs.self.legacyPackages.${system}.deployBatch);
          # mesh: the overlay table validates and derives dns names
          assert mesh.members.web-mesh.dnsName == "web.mesh";
            pkgs.runCommand "mandala-showcase" {} "echo ok > $out";
      };
    });
}
