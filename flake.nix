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
        topo = self.lib.evalTopology (import ./examples/fake-fleet/topology.nix);
        pki = self.lib.evalPki (import ./examples/fake-fleet/pki.nix);
        member = self.lib.evalMember (import ./examples/fake-fleet/member.nix);
      in
        assert op.gpg.keyIdLong == "89ABCDEF01234567";
        assert op.gpg.keyIdShort == "01234567";
        assert op.gpg.openpgp4fpr == "openpgp4fpr:0123456789ABCDEF0123456789ABCDEF01234567";
        assert topo.vlans.mgmt.prefixLength == 24;
        assert topo.vlans.storage.subnet == null;
        assert topo.vlans.storage.prefixLength == null;
        assert pki.cas.example-intermediate.signedBy == "example-root";
        assert builtins.length (builtins.attrNames pki.cas) == 2;
        assert member.fqdn == "example-node.example.test";
        assert member.architecture == "armv7l"; # derived from build.system
        
        assert member.zerotier.memberId == "0123456789";
        assert member.zerotier.name == null;
        assert member.deployment.ssh.host == "example-node.example.test";
        assert !member.deployment.deployRs.enable; # facts-only by default
        
        assert self.lib.groupsFor member
        == ["nixos" "armv7l" "server" "cache" "edge" "fake" "fake_extra_group"];
          pkgs.runCommand "mandala-fake-fleet" {} "echo ok > $out";
    });
  };
}
