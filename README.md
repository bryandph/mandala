# mandala

Fleet contract engine: schema + projection library for describing a
heterogeneous fleet (operator identity, network topology, class-agnostic
members) and projecting that single source outward to the authorities that
manage it (deploy tooling, ansible inventory, sops recipients, DNS/DHCP
records, overlay-network members).

Mandala is the **generic engine**: it defines what a fleet description looks
like and how to compute projections from one. It deliberately contains no
fleet — no VLAN, no key, no address. An operator publishes their own *data
flake* (the operator instance) that fills this schema, and consumers pin that
single input to get validated data, the schema modules, and this library
together.

```
mandala            engine (this repo): schema + lib, deps = nixpkgs.lib only
└── <operator>     data flake: values filling the schema (e.g. mandala-bph)
    └── consumers  infra flakes pin the data flake and read projections
```

## Outputs

- `lib.schemas.<name>` — module paths declaring the contract
  (`operator`; topology and member schemas to follow).
- `lib.evalOperator data` — validate operator data against the schema and
  return it with derived fields (GPG key ids derived from the one authored
  full fingerprint).
- `checks.<system>.fake-fleet` — the engine evaluated against the bundled
  fake fleet in `examples/fake-fleet/`.

## Design

Configs author the inventory; projections flow outward. The engine never
generates host configurations from inventory — it validates facts the
configurations (and facts-only members) declare, and computes the views other
systems consume.
