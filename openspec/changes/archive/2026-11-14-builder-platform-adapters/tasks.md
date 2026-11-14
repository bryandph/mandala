## Tasks

- [x] 1. Record and approve the slice-2 design and delta specification.
- [x] 2. Implement shared pure renderer and opt-in platform exports.
- [x] 3. Add exact goldens and adapter evaluation/ownership/safety tests.
- [x] 4. Document option priorities, adoption, inspection and limitations.
- [x] 5. Run targeted checks, compatibility goldens, formatting and CI commands.

## Verification record

Design approved before implementation. On aarch64-darwin with Nix 2.34.9,
all of the following passed locally:

- `nix build .#checks.aarch64-darwin.builder-renderer`
- `nix build .#checks.aarch64-darwin.builder-adapters`
- `nix build .#checks.aarch64-darwin.builder-ssh-scope`
- `nix build .#checks.aarch64-darwin.builder-schema`
- `nix build .#checks.aarch64-darwin.builder-controllers`
- `nix build .#checks.aarch64-darwin.builder-projection`
- `nix build .#checks.aarch64-darwin.builder-compat`
- `nix build .#checks.aarch64-darwin.fake-fleet`
- The synthetic controller identity expression from `.github/workflows/ci.yml`
- `nix flake check`
- `nix fmt -- --check .`
- `nix flake check ./templates/fleet --override-input mandala . --no-write-lock-file`
- `nix flake check ./examples/showcase --override-input mandala . --no-write-lock-file`
- `nix build ./examples/showcase#network-tf --override-input mandala . --no-write-lock-file`
- `nix build .#mandala-rs`

The existing builder-compat tests and baseline fixtures are untouched. NixOS
module evaluation uses the pinned real modules; Darwin is option-contract
stub evaluation, with its checked source revision cited in docs/builders.md.
Native macOS flake checks do not execute the Linux-only VM test or establish
Darwin activation, deployed Nix lane feature matching, or shared-endpoint slot
accounting. Those remain qualification limitations, not claims of these tests.
