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

    # The Python porcelain (TUI tiers + cores; the headless CLI/MCP moved
    # to the Rust binary below), built from nixpkgs only (the purity
    # invariant holds: no new inputs). Exposed twice: `mandala-fleet-python`
    # is the composable python MODULE — an operator devshell builds a
    # python3.withPackages env from it — and `mandala-cli` is the
    # standalone application. Lazy eval: lib-only consumers never
    # instantiate either.
    packages = eachSystem (pkgs: rec {
      mandala-fleet-python = pkgs.python3Packages.buildPythonPackage {
        pname = "mandala-fleet";
        version = "0.1.0";
        pyproject = true;
        src = ./cli;
        build-system = [pkgs.python3Packages.setuptools];
        dependencies = with pkgs.python3Packages; [typer textual pyte fastmcp];
        # The runner demux / selector-resolution tests are the headless
        # half of the TUI verification — they gate the package build.
        nativeCheckInputs = [pkgs.python3Packages.pytestCheckHook];
        pythonImportsCheck = ["mandala_fleet"];
      };
      mandala-cli = pkgs.python3Packages.toPythonApplication mandala-fleet-python;

      # The Rust porcelain (OpenSpec change mandala-rust-rewrite, phase 1).
      # buildRustPackage — NOT crane — because the purity invariant above
      # (no inputs beyond nixpkgs) rules out crane, which is a flake input;
      # buildRustPackage ships in nixpkgs. `cargoLock.lockFile` vendors the
      # LOCAL workspace deterministically from the checked-in Cargo.lock (no
      # cargoHash, which is for third-party fetches). The source is fileset-
      # narrowed to the Cargo manifests + crate trees so a Python-only edit
      # never rebuilds the Rust binary and vice versa.
      mandala-rs = pkgs.rustPlatform.buildRustPackage {
        pname = "mandala";
        version = "0.1.0";
        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./crates
            # The MCP golden fixtures are the parity oracle the Rust server's
            # check-phase test (`crates/mandala-mcp/tests/parity.rs`) replays —
            # one copy, shared with the Python capture script, never duplicated.
            ./cli/tests/fixtures/mcp
            # The interop golden fixtures (fleet-state-formats): a state dir
            # written by the Python implementation, read by the direction-A
            # cargo tests (`crates/mandala-core/src/interop_tests.rs`).
            ./cli/tests/fixtures/interop
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

        # The interop helper is test tooling (the `mandala-interop` check's
        # direction-B driver), not operator surface: keep it out of bin/ so
        # the package exposes only the porcelain + its eval worker.
        postInstall = ''
          mkdir -p $out/libexec/mandala
          mv $out/bin/mandala-interop-helper $out/libexec/mandala/
        '';

        # cargo test over the workspace runs in the check phase (the unit
        # tests in each crate) — the Rust half of the package gate, the
        # mirror of `mandala-cli`'s pytestCheckHook.
        meta = {
          description = "mandala fleet porcelain (Rust) — CLI + stdio MCP, single static binary";
          mainProgram = "mandala";
          license = lib.licenses.mit;
        };
      };

      default = mandala-cli;
    });

    # Evaluate the engine against the bundled fake fleet: proves the schema
    # validates and the derived fields compute correctly without any
    # operator-specific value entering this repo.
    checks = eachSystem (pkgs: {
      # Cross-implementation interop gate (fleet-state-formats spec, OpenSpec
      # change mandala-rust-rewrite task 2.5): the two toolchains meet HERE.
      # Direction A (Python-written fixtures read by Rust) already runs in
      # `mandala-rs`'s cargo-test check phase — its fixture tree is checked
      # in — so this check drives the OTHER half: pytest attaches the Python
      # `registry.open_run`/`DeployRun.attach` to runs produced by the REAL
      # Rust runners (the `mandala-interop-helper` under libexec/, payloads
      # all trivial `sh -c` — no ansible/nix/network). Purity invariant
      # holds: python env + runCommand are nixpkgs-only.
      mandala-interop = let
        p = self.packages.${pkgs.stdenv.hostPlatform.system};
        pyEnv = pkgs.python3.withPackages (ps: [p.mandala-fleet-python ps.pytest]);
      in
        pkgs.runCommand "mandala-interop" {
          nativeBuildInputs = [pyEnv];
          MANDALA_RS_INTEROP_BIN = "${p.mandala-rs}/libexec/mandala/mandala-interop-helper";
        } ''
          export HOME=$TMPDIR
          pytest -p no:cacheprovider -v \
            ${./cli/tests}/test_interop_rs.py \
            ${./cli/tests}/test_interop_fixtures.py
          touch $out
        '';

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
      in
        assert op.gpg.keyIdLong == "89ABCDEF01234567";
        assert op.gpg.keyIdShort == "01234567";
        assert op.gpg.openpgp4fpr == "openpgp4fpr:0123456789ABCDEF0123456789ABCDEF01234567";
        assert topo.vlans.mgmt.prefixLength == 24;
        assert topo.vlans.storage.subnet == null;
        assert topo.vlans.storage.prefixLength == null;
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
          pkgs.runCommand "mandala-fake-fleet" {} "echo ok > $out";
    });
  };
}
