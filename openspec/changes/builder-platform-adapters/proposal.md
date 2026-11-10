## Why

The pure builder projection is inspectable but does not install daemon-visible
configuration. Consumers need opt-in adapters without duplicating route rendering.

## What changes

Export NixOS and nix-darwin builder modules, a shared pure renderer, exact golden
checks, and adoption documentation. Preserve every slice-1 output.

## Non-goals

Consumer migration, live qualification, runtime overrides, deployment identity
precedence, new flake inputs, and global resource scheduling are excluded.
