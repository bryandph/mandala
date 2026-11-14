# Tasks

- [x] Resolve omitted CA principals after connection precedence; preserve explicit CA and pin behavior.
- [x] Recast restored examples/fixtures as non-VM external builders without losing jump/port/alias/lane coverage.
- [x] Add short-name CA renderer golden and projection/failure tests; leave compatibility goldens unchanged.
- [x] Run `nix build .#checks.<system>.builder-schema .#checks.<system>.builder-controllers .#checks.<system>.builder-projection .#checks.<system>.builder-compat .#checks.<system>.builder-renderer .#checks.<system>.builder-adapters .#checks.<system>.builder-ssh-scope .#checks.<system>.fake-fleet`.
- [x] Run `nix fmt -- --check .` and all local executable CI steps from `.github/workflows/ci.yml`.
