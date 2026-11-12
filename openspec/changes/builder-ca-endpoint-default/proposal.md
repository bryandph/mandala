# Builder CA endpoint defaults

## Why

CA-signed host certificates commonly include the short name used to dial a
builder. Requiring the same principal twice adds avoidable authoring overhead.

## What changes

Default omitted CA principals to the resolved connection hostname. Preserve
explicit principals and strict pin/CA trust. Recast builder examples as non-VM
external builders, retaining jump routes, ports and lane coverage.

## Non-goals

No new trust kind, trust discovery, VM targeting/lifecycle, consumer migration,
runtime builder overrides, deployment changes or flake input changes.
