{
  description = "Mandala — fleet contract engine: operator/topology/member schema + projection lib";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-26.05";

  outputs = {
    self,
    nixpkgs,
  }: let
    inherit (nixpkgs) lib;
    systems = ["aarch64-darwin" "aarch64-linux" "x86_64-darwin" "x86_64-linux"];
    eachSystem = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
  in {
    # The engine: schema modules + evaluators. Depends on nixpkgs.lib ONLY —
    # consumers get no transitive package closure from mandala. Operator
    # values live in a separate data flake (e.g. mandala-bph) that fills
    # this schema; nothing in this repo names a real fleet.
    lib = import ./lib {inherit lib;};

    formatter = eachSystem (pkgs: pkgs.alejandra);

    # Evaluate the engine against the bundled fake fleet: proves the schema
    # validates and the derived fields compute correctly without any
    # operator-specific value entering this repo.
    checks = eachSystem (pkgs: {
      fake-fleet = let
        op = self.lib.evalOperator (import ./examples/fake-fleet/operator.nix);
      in
        assert op.gpg.keyIdLong == "89ABCDEF01234567";
        assert op.gpg.keyIdShort == "01234567";
        assert op.gpg.openpgp4fpr == "openpgp4fpr:0123456789ABCDEF0123456789ABCDEF01234567";
          pkgs.runCommand "mandala-fake-fleet" {} "echo ok > $out";
    });
  };
}
