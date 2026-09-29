## MODIFIED Requirements

### Requirement: Layered repo with a purity invariant
The mandala engine flake SHALL declare exactly three flake inputs: nixpkgs,
and the structural inputs `flake-parts` and `import-tree`. `flake-parts`
SHALL follow the engine's nixpkgs for its library input, and consumers SHALL
be able to override all three through `follows`. The structural inputs
SHALL be used only to define and export module structure (role modules and
flake-parts wiring); delivery tools and toolchains SHALL NOT be engine
inputs. Layers SHALL be gated by allowed dependencies: `schema/` modules
import nothing, `lib/` uses only `nixpkgs.lib`, `flake-modules/` and
`nixos-modules/` are exported as module PATHS, `ansible/` and `cli/` are
content/packages built from nixpkgs, and only `examples/` and contract
fixtures may pin other third-party flakes.

#### Scenario: lib-only consumers stay weightless
- **WHEN** a consumer pins mandala and evaluates only `lib`/`schemas`
- **THEN** the evaluated code references no input other than
  `nixpkgs.lib`, no package derivations (CLI, collection) are
  instantiated, and neither structural input is required by that code

#### Scenario: toolchains are injected, never pinned
- **WHEN** `lib.projections.deployNodes` is called
- **THEN** deploy-rs and nixpkgs arrive as function ARGUMENTS from the
  caller, and the engine's flake.lock contains no deploy-rs entry

#### Scenario: engine lock is limited to structural inputs
- **WHEN** the engine's flake.lock is inspected
- **THEN** its root inputs are exactly nixpkgs, flake-parts and
  import-tree, and no delivery tool (MCP delivery, formatter, devenv,
  Home Manager, nix-darwin) appears

#### Scenario: consumer overrides structure
- **WHEN** a consumer sets `mandala.inputs.flake-parts.follows` and
  `mandala.inputs.import-tree.follows` to its own inputs
- **THEN** the role modules evaluate against the consumer's copies and the
  consumer's lock contains a single copy of each
