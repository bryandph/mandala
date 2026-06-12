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
        mesh = self.lib.evalMesh (import ./examples/fake-fleet/mesh.nix);

        # Projection fixtures: the fake member with its management surfaces
        # flipped on (the factory's job in a real fleet), plus a facts-only
        # member that must NOT appear in the inventory.
        managedMember = self.lib.evalMember (lib.recursiveUpdate (import ./examples/fake-fleet/member.nix) {
          deployment.ansible.enable = true;
          deployment.deployRs.enable = true;
        });
        factsOnly = self.lib.evalMember {name = "facts-only";};

        # NixOS member on a cloud venue: platform names the VENUE, the
        # consumer-built closure (build != null) marks it NixOS.
        cloudMember = self.lib.evalMember {
          name = "cloud-node";
          platform = "hetzner";
          build.system = "x86_64-linux";
          deployment.ansible.enable = true;
        };

        inventory = self.lib.projections.ansibleInventory {
          hosts = {
            example-node = managedMember;
            facts-only = factsOnly;
            cloud-node = cloudMember;
          };
          extraHostvars = name: {
            example_dir = "fleet/${name}";
            # Hooks merge after engine defaults, so they can override them.
            ansible_user = "operator";
          };
        };
        hv = inventory.all.hosts.example-node;

        # Conventions are overridable: no interpreter pin, no guard group.
        bareInventory = self.lib.projections.ansibleInventory {
          hosts = {example-node = managedMember;};
          pythonInterpreter = null;
          guardGroup = null;
        };

        sopsCfg = self.lib.projections.sopsConfig {
          operatorAnchor = op.gpg.fingerprint;
          recipients = {
            example-node = "age1zzzfakefakefakefakefakefakefakefakefakefakefakefakefake0node";
            other-node = "age1aaafakefakefakefakefakefakefakefakefakefakefakefakefakeother";
          };
          rules = [
            {
              path = "secrets/admin.yaml";
              adminOnly = true;
            }
            {
              path = "secrets/all.yaml";
              readers = ["other-node" "example-node"];
            }
            {
              path = "secrets/one.yaml";
              readers = ["example-node" "example-node"];
            }
          ];
        };
        ruleAt = n: lib.head (lib.elemAt sopsCfg.creation_rules n).key_groups;

        # evalSecrets fixtures: a fleet of one keyed member + one keyless
        # member, and a declaration set exercising every reader form.
        keyedMember = self.lib.evalMember (lib.recursiveUpdate (import ./examples/fake-fleet/member.nix) {
          deployment.sops.recipient = "age1zzzfakefakefakefakefakefakefakefakefakefakefakefakefake0node";
        });
        keylessMember = self.lib.evalMember {name = "keyless";};
        secretsFleet = {
          example-node = keyedMember;
          keyless = keylessMember;
        };

        secretsEval = self.lib.evalSecrets {
          hosts = secretsFleet;
          declarations = {
            host-keys = {
              path = "secrets/host-keys.yaml";
              readers.adminOnly = true;
              custody.keySource = "generated";
            };
            cache-creds = {
              path = "secrets/cache.yaml";
              readers.groups = ["cache"]; # example-node's role group; keyless is not in it
            };
            user = {
              path = "secrets/user.yaml";
              readers.all = true; # the sops-identity set — keyless is excluded, not an error
            };
          };
        };

        failsEval = decls:
          !(builtins.tryEval (self.lib.evalSecrets {
            hosts = secretsFleet;
            declarations = decls;
          })).success;

        # The declarations-driven projection: rules derive from evalSecrets
        # output (adminOnly block first, each block path-sorted).
        sopsFromDecls = self.lib.projections.sopsConfig {
          operatorAnchor = op.gpg.fingerprint;
          recipients = {
            example-node = "age1zzzfakefakefakefakefakefakefakefakefakefakefakefakefake0node";
          };
          secrets = secretsEval;
        };
        declRuleAt = n: lib.head (lib.elemAt sopsFromDecls.creation_rules n).key_groups;
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
        
        assert mesh.members.example-node-mesh.dnsName == "example-node.mesh";
        assert mesh.members.example-phone.name == null;
        assert mesh.members.example-phone.dnsName == null;
        assert member.deployment.ssh.host == "example-node.example.test";
        assert !member.deployment.deployRs.enable; # facts-only by default
        
        assert self.lib.groupsFor member
        == ["nixos" "armv7l" "server" "cache" "edge" "fake" "fake_extra_group"];
        # ansibleInventory: membership, hostvars, groups, guard.
        assert builtins.attrNames inventory.all.hosts == ["cloud-node" "example-node"]; # facts-only filtered out
        
        assert inventory.all.hosts.cloud-node.ansible_python_interpreter
        == "/run/current-system/sw/bin/python3"; # cloud venue platform, still NixOS
        
        assert hv.ansible_host == "example-node.example.test";
        assert hv.ansible_user == "operator"; # extraHostvars overrides the convention default
        
        assert hv.ansible_python_interpreter == "/run/current-system/sw/bin/python3";
        assert hv.example_dir == "fleet/example-node";
        assert !(hv ? ansible_port); # default port 22 emits no var
        
        assert inventory.all.children.fake.hosts == {example-node = null;};
        assert inventory.all.children.fake_extra_group.hosts == {example-node = null;};
        assert inventory.all.children.deploy_rs.hosts == {example-node = null;};
        assert !(bareInventory.all.hosts.example-node ? ansible_python_interpreter);
        assert !(bareInventory.all.children ? deploy_rs);
        # sopsConfig: keys ordering, admin-only rule shape, recipient sets.
        assert lib.head sopsCfg.keys == op.gpg.fingerprint;
        assert lib.length sopsCfg.keys == 3; # anchor + one age key per member, sorted by name
        
        assert (lib.elemAt sopsCfg.creation_rules 0).path_regex == "secrets/admin\\.yaml$";
        assert (ruleAt 0).pgp == [op.gpg.fingerprint];
        assert !(ruleAt 0 ? age); # adminOnly: NO age key, not age = []
        
        assert lib.length (ruleAt 1).age == 2; # sorted by member name
        
        assert (ruleAt 2).age == ["age1zzzfakefakefakefakefakefakefakefakefakefakefakefakefake0node"]; # readers unique'd
        
        # evalSecrets: reader resolution + every cross-field invariant as a
        # negative test (each must FAIL eval, not silently mis-seal).
        assert secretsEval.host-keys.resolvedReaders == [];
        assert secretsEval.cache-creds.resolvedReaders == ["example-node"]; # group-resolved
        
        assert secretsEval.user.resolvedReaders == ["example-node"]; # `all` = sops-identity set
        
        assert secretsEval.host-keys.custody.keySource == "generated";
        assert failsEval {
          a = {path = "secrets/dup.yaml";};
          b = {path = "secrets/dup.yaml";};
        }; # unique paths
        
        assert failsEval {
          bad = {
            path = "secrets/bad.yaml";
            readers.adminOnly = true;
            readers.members = ["example-node"];
          };
        }; # adminOnly exclusivity
        
        assert failsEval {
          bad = {
            path = "secrets/bad.yaml";
            readers.members = ["ghost"];
          };
        }; # unknown member
        
        assert failsEval {
          bad = {
            path = "secrets/bad.yaml";
            readers.groups = ["nonexistent-group"];
          };
        }; # empty group
        
        assert failsEval {
          bad = {
            path = "secrets/bad.yaml";
            readers.members = ["keyless"];
          };
        }; # reader without a recipient
        
        # sopsConfig over declarations: deterministic rule order (adminOnly
        # first, then member rules by path) and resolved recipient sets.
        assert map (r: r.path_regex) sopsFromDecls.creation_rules
        == ["secrets/host-keys\\.yaml$" "secrets/cache\\.yaml$" "secrets/user\\.yaml$"];
        assert !(declRuleAt 0 ? age); # adminOnly declaration → pgp-only rule
        
        assert (declRuleAt 1).age == ["age1zzzfakefakefakefakefakefakefakefakefakefakefakefakefake0node"];
        assert (declRuleAt 2).age == ["age1zzzfakefakefakefakefakefakefakefakefakefakefakefakefake0node"];
          pkgs.runCommand "mandala-fake-fleet" {} "echo ok > $out";
    });
  };
}
