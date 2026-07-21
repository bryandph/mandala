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

    # flake-parts shim, exported as PATHS so the engine flake gains no
    # inputs (flake-parts arrives from the CONSUMER's flake). Wiring +
    # hook options only — every projection these modules emit is callable
    # directly from lib.projections for non-flake-parts consumers.
    flakeModules = {
      fleet = ./flake-modules/fleet.nix;
      ansible = ./flake-modules/ansible.nix;
      sops = ./flake-modules/sops.nix;
      deploy = ./flake-modules/deploy.nix;
      default = ./flake-modules/fleet.nix;
    };

    templates.fleet = {
      path = ./templates/fleet;
      description = "A mandala fleet: one NixOS member, operator skeleton, fleet + ansible flakeModules wired";
    };

    formatter = eachSystem (pkgs: pkgs.alejandra);

    # Rust workspace devshell (OpenSpec change mandala-rust-rewrite).
    # Deliberately a plain nixpkgs mkShell: the purity invariant above
    # (no inputs beyond nixpkgs) rules out the devenv/rust-overlay
    # wiring used elsewhere in the org — and the shell's toolchain is
    # the exact rustPlatform the mandala-rs package derivation builds
    # with, so `cargo build` in dev and `nix build` never skew.
    # The eval-worker spike will extend this with libnixexpr-c
    # (pkgs.nix dev output) + bindgen once the bindings are chosen.
    devShells = eachSystem (pkgs: {
      default = pkgs.mkShell {
        packages = with pkgs; [
          # Toolchain — same nixpkgs rustc as rustPlatform.buildRustPackage
          rustc
          cargo
          clippy
          rustfmt
          rust-analyzer
          # Cargo extensions
          cargo-edit
          cargo-outdated
          cargo-audit
          cargo-hack
          cargo-nextest
          cargo-llvm-cov
          taplo
          # Native build deps (openssl-sys and friends)
          pkg-config
          openssl
          # Nix-side tooling (CI runs `nix fmt -- --check .`)
          alejandra
          statix
          deadnix
          nixd
        ];

        # The eval worker (mandala-eval-worker) links libnixexpr-c /
        # libnixflake-c through the `nix-bindings-sys` FFI: pkg-config finds the
        # stable C API `.pc` files in `nix`'s `dev` output (put it in
        # buildInputs so the pkg-config setup hook paths its dev pkgconfig),
        # and bindgen needs libclang, which `rustPlatform.bindgenHook` wires
        # (LIBCLANG_PATH + BINDGEN_EXTRA_CLANG_ARGS).
        buildInputs = [pkgs.nix];
        nativeBuildInputs = [pkgs.rustPlatform.bindgenHook];

        # nixpkgs rustc ships without the rust-src component;
        # rust-analyzer needs it for std-library navigation.
        env.RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
      };
    });

    # The Rust porcelain (OpenSpec changes mandala-rust-rewrite +
    # mandala-native-tui — the Python package is fully retired).
    # buildRustPackage — NOT crane — because the purity invariant above
    # (no inputs beyond nixpkgs) rules out crane, which is a flake input;
    # buildRustPackage ships in nixpkgs. `cargoLock.lockFile` vendors the
    # LOCAL workspace deterministically from the checked-in Cargo.lock (no
    # cargoHash, which is for third-party fetches). The source is fileset-
    # narrowed to the Cargo manifests + crate trees so an engine-side edit
    # never rebuilds the Rust binary. Lazy eval: lib-only consumers never
    # instantiate it.
    packages = eachSystem (pkgs: rec {
      mandala-rs = pkgs.rustPlatform.buildRustPackage {
        pname = "mandala";
        version = "0.1.0";
        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./crates
          ];
        };
        cargoLock.lockFile = ./Cargo.lock;

        # The eval worker links the stable Nix C API (libnixexpr-c /
        # libnixflake-c) via `nix-bindings-sys`. pkg-config resolves the `-c`
        # `.pc` files from `nix`'s `dev` output (buildInputs → the pkg-config
        # setup hook paths it); bindgenHook supplies libclang for the FFI
        # binding generation. This keeps the flake purity invariant intact —
        # `nix` and clang come from nixpkgs, no new flake inputs; the binding
        # crate itself is a vendored cargo dep in Cargo.lock.
        nativeBuildInputs = [pkgs.pkg-config pkgs.rustPlatform.bindgenHook];
        buildInputs = [pkgs.nix];

        # cargo test over the workspace runs in the check phase (the unit
        # tests in each crate, the inline golden-byte format gates, and the
        # leader-vs-follower parity suite) — the package gate.
        meta = {
          description = "mandala fleet porcelain (Rust) — CLI + stdio MCP, single static binary";
          mainProgram = "mandala";
          license = lib.licenses.mit;
        };
      };

      default = mandala-rs;
    });

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
        failsDeep = value: !(builtins.tryEval (builtins.deepSeq value true)).success;

        # Projection fixtures: the fake member with its management surfaces
        # flipped on (the factory's job in a real fleet), plus a facts-only
        # member that must NOT appear in the inventory.
        managedMember = self.lib.evalMember (lib.recursiveUpdate (import ./examples/fake-fleet/member.nix) {
          deployment.ansible.enable = true;
          deployment.deployRs.enable = true;
        });
        factsOnly = self.lib.evalMember {name = "facts-only";};
        fullDeploySettingsMember = self.lib.evalMember {
          name = "full-deploy-settings";
          deployment.deployRs = {
            autoRollback = false;
            fastConnection = false;
            magicRollback = false;
            confirmTimeout = 0;
            activationTimeout = 65535;
            tempPath = "/var/tmp/mandala";
            sudo = "doas -u";
            user = "deployer";
            sshOpts = ["-o" "ControlMaster=auto"];
          };
        };
        taxonomyMember = self.lib.evalMember {
          name = "taxonomy-member";
          tags = ["edge-compute"];
          deployment = {
            ssh.port = 2222;
            deployRs = {
              enable = true;
              sshOpts = ["-o" "Member=yes"];
            };
          };
        };
        evalFleetModule = {
          deployment ? {},
          members ? {taxonomy-member = taxonomyMember;},
        }:
          lib.evalModules {
            specialArgs.inputs.self.nixosConfigurations = {};
            modules = [
              ./flake-modules/fleet.nix
              {
                options.flake.mandala = lib.mkOption {
                  type = lib.types.raw;
                };
                config.mandala = {
                  extraMembers = members;
                  inherit deployment;
                };
              }
            ];
          };
        fleetModuleEval = evalFleetModule {
          deployment = {
            settings = {
              autoRollback = false;
              activationTimeout = 600;
              sshOpts = ["-o" "Fleet=yes"];
            };
            groupSettings.edge_compute = {
              confirmTimeout = 45;
              fastConnection = false;
              magicRollback = false;
              sshOpts = ["-o" "Group=yes"];
            };
          };
        };
        fleetDeployment = fleetModuleEval.config.mandala.deployment;
        fleetDeploySettings =
          fleetModuleEval.config.flake.mandala.projections.deploy.settings."taxonomy-member";
        fleetDeploySettingsExpected = {
          activation = "switch";
          activationTimeout = 600;
          autoRollback = false;
          confirmTimeout = 45;
          fastConnection = false;
          hostname = "taxonomy-member";
          magicRollback = false;
          sshOpts = [
            "-p"
            "2222"
            "-o"
            "Member=yes"
            "-o"
            "Group=yes"
            "-o"
            "Fleet=yes"
          ];
          sshUser = "root";
        };

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

        # Aggregate contract output: the lib function the fleet flakeModule
        # wires (the module itself needs flake-parts, which the engine
        # doesn't pin — nixspace + the template are its integration tests).
        agg = self.lib.aggregate {
          members = {
            example-node = managedMember;
            facts-only = factsOnly;
          };
          operator = op;
          projections = {ansibleInventory = inventory;};
        };

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

        deploySettingsGolden = self.lib.mergeDeploySettings {
          knownGroups = ["alpha" "zeta"];
          fleet = {
            activationTimeout = 600;
            confirmTimeout = 30;
            sshOpts = ["-o" "Fleet=yes"];
          };
          groupSettings = {
            alpha = {
              confirmTimeout = 40;
              magicRollback = false;
              sshOpts = ["-o" "Alpha=yes"];
            };
            zeta = {
              confirmTimeout = 50;
              magicRollback = true;
              sshOpts = ["-o" "Zeta=yes"];
            };
          };
          # Deliberately unsorted input: the merge function owns ordering.
          memberGroups = ["zeta" "alpha"];
          member = {
            confirmTimeout = 90;
            sshOpts = ["-o" "Member=yes"];
          };
        };
        deploySettingsExpected = {
          activationTimeout = 600;
          autoRollback = true;
          confirmTimeout = 90;
          fastConnection = true;
          magicRollback = true;
          sshOpts = [
            "-o"
            "Member=yes"
            "-o"
            "Alpha=yes"
            "-o"
            "Zeta=yes"
            "-o"
            "Fleet=yes"
          ];
        };
        deploySettingsSiblingWinner = self.lib.mergeDeploySettings {
          knownGroups = ["alpha" "zeta"];
          groupSettings = {
            alpha.confirmTimeout = 40;
            zeta.confirmTimeout = 50;
          };
          memberGroups = ["zeta" "alpha"];
        };
        deploySettingsAbsentValues = self.lib.mergeDeploySettings {
          knownGroups = ["alpha"];
          fleet = {
            confirmTimeout = 30;
            magicRollback = null;
            sshOpts = [];
          };
          groupSettings.alpha = {
            activationTimeout = 600;
            confirmTimeout = null;
            sshOpts = [];
          };
          memberGroups = ["alpha"];
          member = {
            confirmTimeout = null;
            tempPath = null;
            sshOpts = [];
          };
        };
        deploySettingsCompat = {
          autoRollback = true;
          fastConnection = true;
          sshOpts = ["-p" "22"];
        };
        unknownDeployGroupFails =
          !(builtins.tryEval (builtins.deepSeq (self.lib.mergeDeploySettings {
              knownGroups = ["known"];
              groupSettings.ghost.confirmTimeout = 30;
              memberGroups = ["known"];
            })
            true)).success;
      in
        assert op.gpg.keyIdLong == "89ABCDEF01234567";
        assert op.gpg.keyIdShort == "01234567";
        assert op.gpg.openpgp4fpr == "openpgp4fpr:0123456789ABCDEF0123456789ABCDEF01234567";
        assert topo.vlans.mgmt.prefixLength == 24;
        assert topo.vlans.storage.subnet == null;
        assert topo.vlans.storage.prefixLength == null;
        assert failsDeep (self.lib.evalTopology {vlans.bad.id = 5000;});
        assert failsDeep (self.lib.evalTopology {
          vlans.bad = {
            id = 10;
            subnet = "999.1.2.0/24";
          };
        });
        assert failsDeep (self.lib.evalTopology {
          vlans.bad = {
            id = 10;
            ula = "not-an-ipv6-prefix";
          };
        });
        # lib.net: id → v4/ULA derivation (digit-mirror convention).
        assert (self.lib.net.forTopology topo).address "mgmt" 102 == "10.0.2.102";
        assert (self.lib.net.forTopology topo).ula "mgmt" 102 == "fd00:dead:beef:2::102";
        assert (self.lib.net.forTopology topo).host "mgmt" 7
        == {
          vlan = "mgmt";
          id = 7;
          address = "10.0.2.7";
          ula = "fd00:dead:beef:2::7";
        };
        # /16: v4 id spans two octets; the ULA group is the id itself.
        assert self.lib.net.v4 {
          subnet = "10.0.0.0/16";
          prefixLength = 16;
        }
        261
        == "10.0.1.5";
        assert self.lib.net.ula {
          subnet = "10.0.0.0/16";
          prefixLength = 16;
          ula = "fd00:dead:beef:5::/64";
        }
        261
        == "fd00:dead:beef:5::261";
        # v4-authored attachments recover the id, then derive the same ULA.
        assert self.lib.net.ulaFromV4 {
          subnet = "10.0.2.0/24";
          prefixLength = 24;
          ula = "fd00:dead:beef:2::/64";
        } "10.0.2.102"
        == "fd00:dead:beef:2::102";
        # evalMemberWith realizes id-authored attachments against topology.
        assert (lib.head
          (self.lib.evalMemberWith topo {
            name = "id-authored";
            networks = [
              {
                vlan = "mgmt";
                id = 16;
              }
            ];
          }).networks).ula
        == "fd00:dead:beef:2::16";
        assert pki.cas.example-intermediate.signedBy == "example-root";
        assert builtins.length (builtins.attrNames pki.cas) == 2;
        assert failsDeep (self.lib.evalPki {
          cas.intermediate = {
            kind = "intermediate";
            pem = "-----BEGIN CERTIFICATE----- invalid";
          };
        });
        assert failsDeep (self.lib.evalPki {
          cas = {
            one = {
              kind = "intermediate";
              signedBy = "two";
              pem = "-----BEGIN CERTIFICATE----- invalid";
            };
            two = {
              kind = "intermediate";
              signedBy = "one";
              pem = "-----BEGIN CERTIFICATE----- invalid";
            };
          };
        });
        assert member.fqdn == "example-node.example.test";
        assert member.architecture == "armv7l"; # derived from build.system
        
        assert failsDeep (self.lib.evalMember {name = "fqdn.example";});
        assert failsDeep (self.lib.evalMember {
          name = "duplicate-role";
          networks = [
            {
              vlan = "one";
              roles = ["dns"];
            }
            {
              vlan = "two";
              roles = ["dns"];
            }
          ];
        });
        assert failsDeep (self.lib.evalMember {
          name = "bad-reservation";
          networks = [
            {
              vlan = "one";
              assignment = "reservation";
            }
          ];
        });
        assert mesh.members.example-node-mesh.dnsName == "example-node.mesh";
        assert mesh.members.example-phone.name == null;
        assert mesh.members.example-phone.dnsName == null;
        assert member.deployment.ssh.host == "example-node.example.test";
        assert !member.deployment.deployRs.enable; # facts-only by default
        
        # Deploy-rs settings remain absent-equivalent until authored; the
        # flattening merge restores legacy effective defaults where required.
        assert member.deployment.deployRs.autoRollback == null;
        assert member.deployment.deployRs.fastConnection == null;
        assert member.deployment.deployRs.magicRollback == null;
        assert member.deployment.deployRs.confirmTimeout == null;
        assert member.deployment.deployRs.activationTimeout == null;
        assert member.deployment.deployRs.tempPath == null;
        assert member.deployment.deployRs.sudo == null;
        assert member.deployment.deployRs.user == null;
        assert member.deployment.deployRs.sshOpts == [];
        assert fullDeploySettingsMember.deployment.deployRs.autoRollback == false;
        assert fullDeploySettingsMember.deployment.deployRs.fastConnection == false;
        assert fullDeploySettingsMember.deployment.deployRs.magicRollback == false;
        assert fullDeploySettingsMember.deployment.deployRs.confirmTimeout == 0;
        assert fullDeploySettingsMember.deployment.deployRs.activationTimeout == 65535;
        assert fullDeploySettingsMember.deployment.deployRs.tempPath == "/var/tmp/mandala";
        assert fullDeploySettingsMember.deployment.deployRs.sudo == "doas -u";
        assert fullDeploySettingsMember.deployment.deployRs.user == "deployer";
        assert fullDeploySettingsMember.deployment.deployRs.sshOpts == ["-o" "ControlMaster=auto"];
        assert failsDeep (self.lib.evalMember {
          name = "negative-deploy-timeout";
          deployment.deployRs.confirmTimeout = -1;
        });
        assert failsDeep (self.lib.evalMember {
          name = "remote-build-is-not-supported";
          deployment.deployRs.remoteBuild = true;
        });
        # Fleet/group settings use the same optional deploy-rs vocabulary.
        assert fleetDeployment.settings.autoRollback == false;
        assert fleetDeployment.settings.activationTimeout == 600;
        assert fleetDeployment.settings.magicRollback == null;
        assert fleetDeployment.settings.sshOpts == ["-o" "Fleet=yes"];
        assert fleetDeployment.groupSettings.edge_compute.confirmTimeout == 45;
        assert fleetDeployment.groupSettings.edge_compute.fastConnection == false;
        assert fleetDeployment.groupSettings.edge_compute.magicRollback == false;
        assert fleetDeployment.groupSettings.edge_compute.sshOpts == ["-o" "Group=yes"];
        assert fleetDeploySettings == fleetDeploySettingsExpected;
        assert builtins.attrNames fleetModuleEval.config.flake.mandala.projections.deploy.settings
        == ["taxonomy-member"];
        assert failsDeep ((evalFleetModule {
          deployment.groupSettings.ghost.confirmTimeout = 30;
        }).config.mandala.members);
        # The authored key must use the sanitized taxonomy spelling.
        assert failsDeep ((evalFleetModule {
          deployment.groupSettings."edge-compute".confirmTimeout = 30;
        }).config.mandala.members);
        assert failsDeep ((evalFleetModule {
          deployment.settings.remoteBuild = true;
        }).config.mandala.deployment.settings);
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
        
        # aggregate: version gate, taxonomy groups over ALL members
        # (facts-only included — surfaces apply their own enable filters),
        # operator + projections carried through as data.
        assert agg.schemaVersion == 1;
        assert builtins.attrNames agg.members == ["example-node" "facts-only"];
        assert agg.groups.cache == ["example-node"];
        assert agg.groups.server == ["example-node" "facts-only"];
        assert agg.operator.gpg.keyIdShort == "01234567";
        assert agg.projections.ansibleInventory.all.hosts ? example-node;
        # sopsConfig over declarations: deterministic rule order (adminOnly
        # first, then member rules by path) and resolved recipient sets.
        assert map (r: r.path_regex) sopsFromDecls.creation_rules
        == ["secrets/host-keys\\.yaml$" "secrets/cache\\.yaml$" "secrets/user\\.yaml$"];
        assert !(declRuleAt 0 ? age); # adminOnly declaration → pgp-only rule
        
        assert (declRuleAt 1).age == ["age1zzzfakefakefakefakefakefakefakefakefakefakefakefakefake0node"];
        assert (declRuleAt 2).age == ["age1zzzfakefakefakefakefakefakefakefakefakefakefakefakefake0node"];
        # Native-deploy 1.3: hand-written merge goldens. Member wins over
        # sorted groups, later group wins over earlier, sshOpts append from
        # inner to outer, unknown group keys fail, and member-only data is
        # byte-shape compatible.
        assert deploySettingsGolden == deploySettingsExpected;
        assert deploySettingsSiblingWinner.confirmTimeout == 50;
        assert deploySettingsAbsentValues
        == {
          activationTimeout = 600;
          autoRollback = true;
          confirmTimeout = 30;
          fastConnection = true;
        };
        assert unknownDeployGroupFails;
        assert (self.lib.mergeDeploySettings {
          knownGroups = [];
          member = deploySettingsCompat;
        })
        == deploySettingsCompat;
          pkgs.runCommand "mandala-fake-fleet" {} "echo ok > $out";
    });
  };
}
