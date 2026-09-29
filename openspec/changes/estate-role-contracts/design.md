## Context

mandala exposes fleet schemas, pure evaluation functions, flake modules, and a JSON fleet aggregate. The intended estate architecture adds eight roles: `library`, `profiles`, `operator-profile`, `org`, `fleet`, `operator`, `estate`, and `project`.

This slice tests a smaller claim: one project can consume a strictly validated org through a proxy role module and use its Nix mirror policy in a NixOS configuration. Successful evaluation alone does not prove daemon trust or actual cache preference.

## Goals / Non-Goals

Goals: explicit experimental org/project interfaces; source agreement distinct from API compatibility; deterministic mirror selection; a working daemon-level substitution test; unchanged fleet behavior.

Non-goals: the other six role interfaces, operator identity and rosters, simultaneous multi-org composition, generic service catalogs, extensions, non-NixOS adapters, and real-estate migration. The eight-role architecture remains the destination, not a promise that this release implements it.

## Decisions

### D1: Publish two experimental role interfaces

Export only `roles.org` and `roles.project` for this slice. Roles automatically supply their name and contract version; conflicting declarations fail. Both publish JSON metadata at `mandala.role`:

- `name`: `org` or `project`.
- `contractVersion`: integer `1` for the experimental interface.
- `provider`: `{ name, source }`, where `name` identifies the direct library or proxy and `source` is its source-tree `narHash` string.
- `engineSource`: the Mandala source-tree `narHash` string.

`source` metadata is diagnostic provenance, not an authentication or authorization claim. It contains no flake objects, paths, modules, or derivations.

| Role | Required data output | Required conventional output | Role inputs |
| --- | --- | --- | --- |
| org | `mandala.contract.org`: validated org data, including section versions | None | Mandala engine, directly |
| project | `mandala.contract.project`: `{ name, orgName, site }`, where site is null or a selected site code | None; the integration fixture separately exports `nixosConfigurations.contract` | Exactly one org; Mandala role module directly or through a proxy |

The project interface records the explicit org binding and selection; it does not require ordinary application repositories to export a NixOS configuration. The integration fixture separately exports `nixosConfigurations.contract` to exercise the adapter. Devshell, package, and other project capabilities are deferred until their consumers establish the necessary interfaces.

The project module validates its binding and selection without forcing unrelated outputs. Integration checks separately validate the fixture's NixOS configuration through bounded assertions on effective Nix settings. Neither check may recursively force the complete flake or build the system during metadata inspection. The conformance derivation forces all contract data and relevant assertions. Reading role metadata alone is not a claim that all outputs have been validated.

Alternative: export eight role modules with placeholder requirements. Rejected because a role label without a concrete output interface provides little conformance value.

### D2: One org per project configuration

A project selects exactly one org and optionally one site. A second org binding fails with a named error. Separate project fixtures consume acme and globex; no fixture merges their policies or tool pins.

The org flake supplies the nixpkgs input used by the project's NixOS configuration. Projects follow that input explicitly. There is no claim that one machine can reconcile competing organization distributions. Multi-org composition needs a separate design covering authority, conflicts, and identity isolation.

### D3: Keep schema compatibility and source agreement separate

Contract versions are integers. This slice supports version 1 only; consumers check an explicit supported-version set. A later change must define compatibility and deprecation rules before supporting more versions.

Source agreement is an independent composition invariant: the engine providing the project's role module must have the same source-tree `narHash` as the org's engine. Two different engine sources advertising version 1 still fail source agreement. This establishes source equality, not that a particular `follows` spelling was used. It does not claim equality of all transitive inputs.

A canonical project graph has `org`, `proxy`, and `nixpkgs` inputs. `proxy.inputs.mandala.follows = "org/mandala"`; `nixpkgs.follows = "org/nixpkgs"`. Structural inputs follow the selected engine. The proxy re-exports a wrapper that supplies its own provider metadata and the engine metadata; a raw module re-export cannot infer which flake forwarded it.

Compare explicit bindings only, never recursively traverse arbitrary input outputs. Missing metadata produces a named failure. Use lock-backed sources in fixtures; the source descriptor is derived from flake source metadata, not an operator-authored hash. Fixture overrides must point at the candidate implementation, including the engine beneath the org and proxy.

Alternative: compare contract versions only. Rejected because identical API versions do not establish identical source code. Exact source agreement is intentionally stricter than compatibility for this experiment.

### D4: Small, strict, serializable org data

`lib.contract.evalOrg` accepts authored data and returns fully validated JSON-compatible data. Share this evaluated value among adapters within the flake evaluation; do not promise global evaluation-once behavior across separate Nix invocations.

The core is `{ name, domains }`, with a non-empty name and non-empty list of domain strings. No roster or identity data is accepted. Schema validation rejects unknown keys. Two optional sections, each at version 1:

- `mirrors`:
  - `entries`: at most 30 named Nix caches with `url`, non-empty `publicKeys`, and string `labels`.
  - `default`: an ordered, duplicate-free list of entry names.
- `sites`: an org-level site catalog keyed by codes matching `^[A-Z0-9-]+$`. In this slice each site carries only `mirrors.preferred`, an ordered, duplicate-free list of entry names.

References must exist; a site with a non-empty `mirrors.preferred` requires the `mirrors` section. Only HTTP(S) cache URLs are supported; credential-bearing URLs, query strings, and fragments are rejected in this slice so the adapter can safely append priority parameters. Keys must have the Nix public-key syntax. Labels carry arbitrary string keys; all structural fields are strict. No schema functions or extension modules enter exported data.

A selected site's preferred entries precede defaults, with duplicates removed preserving first occurrence. Entries not selected contribute neither URLs nor keys. A site with an empty or absent preferred list inherits defaults. Selecting a site not declared in `sites` fails, whether or not `mirrors` is present. With no `mirrors` section the adapter is a no-op, with or without a selected (declared) site.

Sites sit at the org level, not inside `mirrors`, because the site catalog is shared by later sections (network, time, proxies). Later sections add keys under `sites.<code>` beside `mirrors`, so this slice's data remains valid without a breaking move.

Alternative: nest sites under `mirrors` (`mirrors.sites`). Rejected: it would need a breaking relocation once any second section needs per-site settings. Alternative: introduce network, general site settings, arbitrary mirror kinds, and extensibility now. Deferred because none is necessary to prove this path.

### D5: NixOS owns daemon trust and cache preference

The adapter accepts org data and a site through options and configures the host daemon's `nix.settings`. Effective selected URLs receive explicit priorities `10 + index`. The 30-entry cap on `mirrors.entries` bounds priorities to 10–39, strictly ahead of the default priority 40 of the public NixOS cache. These priorities establish relative order among selected mirrors and ahead of default-priority caches; they do not promise precedence over host caches configured with explicit lower priorities. Use additive option definitions to preserve host defaults and unrelated host settings; do not use `mkForce`.

The adapter adds keys only for selected mirrors. Installing it is an explicit host-administrator decision to trust those signing keys. Labels, site preference, and per-org data separation do not scope what a trusted signing key may sign. Existing host cache policies can affect overall selection; the test proves preference among the fixture caches, not exclusive control over every host cache.

No user-level adapter or automatic elevation is provided. A project cannot grant itself daemon trust. A NixOS VM test uses an ordinary untrusted user and signed fixture caches to prove substitution. Nix does not report which substituter served a path, so each fixture cache is served by its own HTTP server with request logging, and the test asserts from those logs that the `.narinfo` and `.nar` requests for the object reached the preferred cache and that the other cache served no `.nar` for it. A second case requests an unconfigured cache/key and must fail substitution; source builds and other caches must not mask this failure. Test keys are generated inside test execution, never committed as production credentials.

Alternative: assert substituter list order in evaluation only. Rejected because runtime priority and daemon trust determine actual behavior.

### D6: Additive output namespace and dependency footprint

Role metadata lives at `mandala.role`; data lives at `mandala.contract.*`. Keep that location for this experimental interface rather than promise a later flattening. Existing aggregate keys remain unchanged. Existing consumers that do not import a role gain no new mandatory fields.

Mandala root inputs remain limited to nixpkgs and optional structural inputs flake-parts/import-tree. A fixture proxy is not a production profiles implementation. All fixture checks use public or local fictional sources; no real org inputs are required.

## Verification

- Preserve the existing showcase aggregate byte-for-byte and existing checks.
- Fully force JSON data and conformance checks; verify metadata can be inspected without building.
- Evaluate org/project fixtures directly and through a proxy against the candidate engine.
- Reject missing outputs, wrong role/version, two-org bindings, unknown fields, bad references, unknown sites, missing source metadata, and different engine sources with the same contract version.
- Assert deterministic URL priorities, selected keys, default preservation, absent-section behavior, and rejection of a 31st mirror entry.
- Run the NixOS VM test for actual preferred-cache use and missing daemon trust.

## Risks / Open Decisions

- The experimental project interface establishes org binding only. It must not be advertised as a complete contract for project tooling or build outputs.
- Source metadata availability and proxy wrappers need an early executable spike. If the declared graph cannot expose reliable hashes without recursion, revise the design before proceeding; do not silently weaken equality to version comparison.
- Per-role conformance and NixOS configuration construction can form recursive dependencies if checks force their own parent outputs. The spike must demonstrate bounded evaluation.
- NixOS VM tests require a Linux builder with virtualization support. Evaluation-only results cannot substitute for this evidence.
- Multi-org policy composition, roster privacy boundaries, membership lifecycle, and stable version policy remain separate follow-up design work.

## Migration and approval

This is a revision of the proposed plan, not implementation approval. After approval, establish baseline, prove the dependency graph, then build the schema and adapter. No real consumers migrate in this change. Prepare release notes only after checks pass; shipping requires separate authorization.
