# A mandala fleet. Your configurations ARE the inventory: each NixOS
# member authors `host.*` inside its own configuration, and the engine
# validates the declared facts and projects them outward to existing
# tools (ansible here; deploy-rs and sops a comment away).
#
# After `nix flake init -t github:bryandph/mandala#fleet`:
#   nix eval --json .#mandala            # the aggregate fleet contract
#   nix eval --json .#ansibleInventory   # serve this live to ansible
{
  description = "A mandala fleet";

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
    # Toolchains are YOUR pins, never the engine's. Uncomment to project
    # deploy-rs nodes + per-group batch artifacts (and import
    # mandala.flakeModules.deploy below):
    # deploy-rs = {
    #   url = "github:serokell/deploy-rs";
    #   inputs.nixpkgs.follows = "nixpkgs";
    # };
  };

  outputs = inputs @ {
    nixpkgs,
    flake-parts,
    mandala,
    ...
  }:
    flake-parts.lib.mkFlake {inherit inputs;} {
      systems = ["x86_64-linux" "aarch64-linux"];

      imports = [
        mandala.flakeModules.fleet
        mandala.flakeModules.ansible
        # mandala.flakeModules.deploy # needs the deploy-rs input above
        # mandala.flakeModules.sops # plus mandala.sops.{operatorAnchor,declarations}
      ];

      # Operator identity, validated against the operator schema into
      # flake.mandala.operator.
      mandala.operator = mandala.lib.evalOperator (import ./operator.nix);

      flake.nixosConfigurations.example-host = nixpkgs.lib.nixosSystem {
        system = "x86_64-linux";
        modules = [
          mandala.lib.schemas.member
          ./hosts/example-host.nix
        ];
      };

      # Facts-only members (routers, switches, BMCs, …) join the fleet as
      # validated plain data:
      # mandala.extraMembers = {
      #   router = mandala.lib.evalMember {
      #     name = "router";
      #     platform = "opnsense";
      #     category = "gateway";
      #   };
      # };
    };
}
