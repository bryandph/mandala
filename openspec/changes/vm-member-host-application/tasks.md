Implementation is approved. Phase A implements the pure Nix contract only; unchecked backend, frontend and runtime qualification tasks remain future work. No consumer adoption or deployment is authorized.

## 1. Review and schema foundation

- [x] 1.1 Obtain explicit plan approval for schema names, the single `nixos-module` backend, aggregate-version handling and recovery semantics before implementing. Keep the macOS host-application adapter an explicit deferred design item; no consumer migration.
- [x] 1.2 Add `host.kind` and typed nullable `host.vm = { owner; backend; }` with shared local invariants at direct module and `evalMember` entry points. Keep category/platform/build semantics and non-VM defaults unchanged.
- [x] 1.3 Add pure fleet relation/manifest validation and derived sorted `projections.vm.guestsByOwner`. Test missing/self/cyclic/nested/multiple ownership, unmanaged or absent owners, unsupported backend/system pairs, missing configurations/artifacts and contradictory kind/relation.
- [x] 1.4 Add fictional acme/globex fixtures: standalone owner, two NixOS guests, independent owner and unmanaged inventory entry. Include a negative macOS-owner fixture proving unsupported application fails closed, not a Darwin implementation.

## 2. Additive projection and selection contracts

- [x] 2.1 Guard Ansible hosts/hostvars/children/guard groups, deploy-rs nodes, independent static batch entries and builder routes in direct projections and flake/controller-platform outputs. Validate effective true guest SSH enables, including factory defaults; test malformed direct inputs and hook reinsertion attempts.
- [x] 2.2 Preserve VM members and canonical taxonomy plus qualifying DNS/DHCP/docs records; test own addresses are not replaced by owner endpoints. Keep static guest-only groups from silently expanding to owners and explain the canonical-versus-transport group distinction.
- [x] 2.3 Coordinate the VM guard with the builder facet without changing its separate capacity work: reject enabled VM builder declarations, require explicit owner systems/features/lanes and strict trust, preserve owner self-exclusion and no inferred emulation.
- [x] 2.4a Add pure guest-to-owner resolution with deduplication, retained requested identities and all potentially affected siblings; reject conflicting explicit exclusions.
- [ ] 2.4b Preview the same immutable plan through CLI/TUI/MCP approval paths, and require renewed approval on impact/revision changes.
- [x] 2.5a Add pure aggregate version-2 ownership/application-reference output. Prove version-1 non-VM parity and no forced derivations in JSON.
- [x] 2.5b Add frontend negotiation and unsupported-version rejection before effects for VM-aware readers.

## 3. One build graph and one qualified backend

- [x] 3.1 Define a versioned typed `nixos-module` manifest and injected consumer adapter with supported systems, inspect/prepare/apply/verify/restore evidence and bounded readiness. Require observable running guest identity and recoverable prior runtime; do not pin a runtime toolchain.
- [x] 3.2 Wire owner application artifacts to immutable guest closure/image dependencies in the consumer fleet graph without recursive configurations. Validate relation/manifest agreement and retain desired/previous paths through settlement/recovery.
- [x] 3.3 Extend single-batch native resolution/build to owner units; failed guest dependencies block that owner while independent verified artifacts retain best-effort behavior. Test one build invocation, required sibling dependencies and no activation-time evaluation/rebuild.
- [x] 3.4 Apply only through owner transport/settings and the module adapter, selecting prebuilt images or restarting guests without guest SSH/guest-exec. Test exact target/privilege arguments, matching running identity plus readiness, mismatched identities, deadlines and unavailable observations.
- [x] 3.5 Preserve target-owned preflight, boot-only staging and dry activation: inhibitor refusal never restarts guests; staged images are pending; dry activation changes no profile/runtime/image pointer. Test successful staging separately from failed live restart.

## 4. Owner serialization and recoverable runtime state

- [x] 4.1 Add an owner-enforced lock around copy/apply/verify/recover shared across runs/controllers with bounded busy reporting. Barrier-controlled tests must cover sibling requests, separate controllers, owner-plus-guest selection, independent owners and reconciliation after stale/crashed runs.
- [x] 4.2 Persist and retain previous owner profile/boot state and every affected guest generation/image/readiness contract before mutation. Refuse missing prior runtime or persistence/rooting failure; explicitly test first-time creation refusal.
- [x] 4.3 Extend recovery beyond deploy-rs host rollback: restore and verify previous affected owner and guest runtimes/readiness. Test host-only rollback insufficiency, all-restored success, one-sibling failure, disabled auto-rollback and primary plus recovery diagnostics.
- [x] 4.4 Test interruption/timeouts and unknown observations: no detached recovery, prohibited new effects or false rolled-back outcome; block subsequent owner mutation until reconciled. Preserve independent successful owners. State disk/data rollback exclusions in preview and records.

## 5. Durable observations and frontend parity

- [x] 5.1 Add atomic VM owner-unit metadata and owner/guest event envelopes without inventing independent guest SSH hosts. Preserve old records, event versions, unknown-field tolerance and partial-line handling; pin canonical serialization bytes.
- [x] 5.2 Share owner/guest settlement across CLI/TUI/MCP: verified versus pending, preview/not-applied, failed/unrecovered, verified rollback and partial/unknown recovery. Test zero controller rc and host confirmation cannot mask guest failure, and unchanged unrelated host counts.
- [x] 5.3 Test attaching after interruption/restart and reconstructing the approved plan, runtime evidence and recovery diagnostics without evaluation or transport effects.
- [x] 5.4 Extend consumer conventions and backend authoring docs with fictional examples, platform/build distinctions, owner capacity advertisement, unsupported macOS adapter and explicit adoption approval. Do not switch a consumer in this change.

## 6. Effect-isolated qualification and final gates

- [x] 6.1a Add `vm-member-schema` and `vm-projections` checks in `flake.nix` for pure Nix contract suites, including pure owner resolution, without live VMs or network services.
- [x] 6.1b Add `vm-owner-resolution` and `vm-owner-backend` Rust checks for the fake-effect suites. Gate backend apply/verify/restore, per-owner concurrent-run serialization, partial recovery and durable protocol behavior.
- [x] 6.2a Run `nix build .#checks.x86_64-linux.vm-member-schema .#checks.x86_64-linux.vm-projections` for the pure Nix suites.
- [x] 6.2b Run `nix build .#checks.<system>.vm-owner-resolution .#checks.<system>.vm-owner-backend` after the Rust targets exist; these remain proposed additions.
- [ ] 6.3 Run existing `nix build .#checks.<system>.fake-fleet`, `nix build .#packages.<system>.mandala-rs` and `nix develop -c cargo test --workspace` for non-VM parity and workspace/deploy/registry/termination regression coverage. Substitute the actual platform; `flake.nix` owns current target names.
- [ ] 6.4 Run `nix fmt -- --check .` and `nix flake check`; record platform and environmental limits, not unrun-platform success. No effect-isolated check may require a real owner, guest, cache or fleet.
- [ ] 6.5 Run `openspec validate vm-member-host-application --strict` when the CLI is available, validate delta/scenario coverage and scan new artifacts for organization-specific content. Report missing tooling explicitly.
- [ ] 6.6 Return reviewed implementation evidence before any merge, deployment or consumer adoption. A macOS adapter remains a separate designed/approved change; completion of these artifacts does not approve implementation.
