## 1. Baseline and dependency feasibility

- [ ] 1.1 Record existing targeted checks and the showcase JSON aggregate as the parity baseline.
- [ ] 1.2 Build a minimal org → proxy → project graph against the candidate source. Prove narHash metadata availability, provider wrapping, canonical follows paths, and metadata inspection without building or recursive evaluation.
- [ ] 1.3 Demonstrate rejection of different engine sources that advertise the same contract version. Stop for a design revision if source agreement cannot be established as specified.
- [ ] 1.4 Add the `flake-parts` (following nixpkgs) and `import-tree` inputs per the `mandala-engine` delta; document consumer-owned nixpkgs/tool inputs and the `follows` overrides. Recheck existing behavior, confirm `lib`/schemas reference only `nixpkgs.lib`, and confirm the lock's root inputs are exactly the three allowed.

## 2. Org schema and selection

- [ ] 2.1 Implement the strict core, the mirrors version 1 schema (30-entry cap), and the org-level sites version 1 schema in `contract/org/`; implement fully validated serializable `lib.contract.evalOrg`.
- [ ] 2.2 Implement site/default selection, reference validation (including site preferences requiring the mirrors section), URL/key validation, labels, stable deduplication.
- [ ] 2.3 Add `contract-org` checks for minimal and full orgs, unknown keys, invalid entries/references, duplicate selections, a 31st entry, unsupported versions, absent sections, a site with no mirror preferences, a preference without the mirrors section, and unknown sites.

## 3. Experimental org/project interfaces

- [ ] 3.1 Implement only `roles.org` and `roles.project`, exact output shapes, metadata, and bounded conformance checks.
- [ ] 3.2 Add acme/globex fixtures, direct projects, and a neutral proxy wrapper fixture. Do not publish a profiles role or operator template.
- [ ] 3.3 Add `contract-roles` checks for required outputs, conflicting declarations, wrong input role/version, two-org bindings, missing metadata, and JSON serialization.
- [ ] 3.4 Add `contract-proxy` checks for direct/proxy provenance, candidate override propagation, source agreement, and the same-version/different-source failure.

## 4. NixOS adapter and runtime proof

- [ ] 4.1 Implement additive NixOS daemon settings from selected mirrors and keys, with explicit priorities and no trusted-user elevation.
- [ ] 4.2 Add `contract-mirrors` evaluation checks for exact effective settings, default preservation, selected keys only, site fallback, and absent-section behavior.
- [ ] 4.3 Add Linux `contract-mirrors-vm`: two signed fictional caches, each behind its own HTTP server with request logging; keys generated during execution; an ordinary daemon client; preferred-cache use proven from the request logs (`.narinfo` and `.nar` on the preferred cache, no `.nar` on the other); and missing-trust rejection. Prevent builds/other caches from masking results.

## 5. Documentation and verification

- [ ] 5.1 Document the experimental interfaces, exact graph, source equality versus API compatibility, daemon trust boundary, and eight-role destination in `docs/contract.md`.
- [ ] 5.2 Document deferred decisions: general project interface, other roles, multi-org composition, identity lifecycle, roster access boundaries, extensions, and stable compatibility policy.
- [ ] 5.3 Wire candidate fixtures and targeted checks into CI; require a Linux builder with VM support for the runtime proof.
- [ ] 5.4 Run `nix fmt`, `nix build .#checks.<system>.{contract-org,contract-roles,contract-proxy,contract-mirrors}` as individual targets, and the Linux `contract-mirrors-vm` target. Run existing checks and compare showcase JSON to baseline.
- [ ] 5.5 Run `openspec validate estate-role-contracts --strict` and the available organization-neutral check across new public artifacts. Report exact verification evidence and remaining limitations for review.
- [ ] 5.6 Prepare experimental release notes only after verification. Obtain separate authorization before tagging, pushing, merging, or migration.
