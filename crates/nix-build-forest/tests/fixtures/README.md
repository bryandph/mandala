# Recorded Nix build streams

These are zstd-compressed `@nix` internal-JSON streams recorded on
2026-07-21 with Nix 2.34.8+1. They contain no credentials.

- `small`: one successful `runCommand` derivation.
- `medium`: a four-derivation graph (`root -> middle -> leaf-a, leaf-b`).
- `failure`: one derivation whose builder exits 42.

`nom-parity.json` records the black-box nom 2.1.8 outcomes from feeding the
same bytes to `nom --json`. The failure divergence is intentional: nom reports
one generic Nix error but leaves the derivation running; the forest attributes
the terminal failure to the derivation named by Nix's error record.

The corpus tests decompress in memory and never rely on the original store
paths still existing. Their graph reader uses the recorded four-node relevant
shape; missing/GC'd `.drv` behavior is covered separately.
