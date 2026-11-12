## 1. Baseline and builder facet

- [x] 1.1 Capture fake-fleet aggregate, deploy and inventory JSON baselines before adding defaults; add the `builder-compat` check skeleton and prove byte equality against those captured fixtures with `nix build .#checks.<system>.builder-compat`.
- [x] 1.2 Add nullable `host.builder` with capability and connection fields and shared invariants for direct module and evalMember entry points; add positive/default and invalid-type/feature/number cases to `builder-schema`. Run `nix fmt -- --check .` and `nix build .#checks.<system>.builder-schema`.
- [x] 1.3 Normalize default/named lanes with null inheritance and replacement lists; test cleared features, replaced systems, invalid resolved lanes, sorted unique sets and retained shared endpoints in `builder-schema`. Run `nix build .#checks.<system>.builder-schema`.

## 2. Controller contract and route validation

- [x] 2.1 Add the empty-default controllers map, explicit selfMember, member allowlists and optional lane selection. Add `builder-controllers` cases for unknown/missing members, null facets, duplicate references, unknown lanes, empty selection, two same-OS profiles and self/external-builder distinction. Run `nix build .#checks.<system>.builder-controllers`.
- [x] 2.2 Add typed routes and fieldwise overrides, whole trust replacement, string-only absolute identities, pin/CA declarations and jump references. Fully force invalid path/type, missing endpoint/login/key/trust, unknown routes, cycles, alias collision and injection cases in `builder-controllers`. Run `nix fmt -- --check .` and `nix build .#checks.<system>.builder-controllers`.

## 3. Pure projection and additive wiring

- [x] 3.1 Implement one pure normalized controller projection with self exclusion and sorted member/lane output. Add `builder-projection` fixtures for direct/jump nondefault-port builder routes, distinct lanes, explicit lane filtering and two controllers on one OS. Run `nix build .#checks.<system>.builder-projection`.
- [x] 3.2 Project nix.buildMachines-shaped lists and structured SSH/knownHosts, with stable aliases, strict trust policy, nondefault ports, remote-program encoding and full-public-key-text encoding assertions. Test pin and CA representations using synthetic public data only. Run `nix build .#checks.<system>.builder-projection`.
- [x] 3.3 Wire direct `buildMachinesByController` and optional aggregate `projections.builders.byController` from that same model; force JSON-only serialization and exact direct/aggregate list equality. Test absent declarations, facet-only declarations, declared empty profiles and unchanged deploy projections in `builder-compat`. Run `nix build .#checks.<system>.builder-compat`.

## 4. Verification and review

- [x] 4.1 Run `nix fmt -- --check .`; individually run `nix build .#checks.<system>.builder-schema`, `nix build .#checks.<system>.builder-controllers`, `nix build .#checks.<system>.builder-projection`, `nix build .#checks.<system>.builder-compat`, and existing `nix build .#checks.<system>.fake-fleet`; then run `nix flake check`.
- [x] 4.2 Run `openspec validate builder-capacity-projection --strict` when available, audit new public artifacts for organization-specific values, and report check results plus byte-parity evidence for review. Use only fake-fleet fixtures; do not perform live route or scheduler tests.
