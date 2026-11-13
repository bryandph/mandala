## Context

`schema/member.nix` separates organizational category, platform/venue, build metadata and management opt-ins. `flake-modules/fleet.nix` assembles configuration-derived and extra members; `lib/projections/{deploy-nodes,deploy-batch,ansible-inventory}.nix` currently select on enables. Neither `category = "vm"` nor a Linux build system conveys host ownership. `openspec/specs/fleet-deploy/spec.md` requires one selection evaluation, one batch build, and prebuilt activation; `fleet-state-formats` distinguishes installed from running generations. Preserve those boundaries rather than create a guest deployment pipeline.

## Goals and Non-goals

Make VM identity first-class and lifecycle authority owner-scoped. Start with one module-managed backend and verified runtime recovery. Preserve non-VM behavior and effect isolation.

Non-goals are implementation, SSH into guests, host-controlled guest-exec, nested hosting, consumer adoption, disk/data rollback and macOS application support. A Linux builder VM under a macOS owner is a motivating future case, not a supported first-backend pair.

## Decisions

### 1. Separate execution kind from category, platform and construction

Choose `host.kind`, an enum `standalone | vm`, defaulting to `standalone`; choose nullable `host.vm`, whose non-null typed fields are `owner` (one canonical member identifier) and `backend` (initial enum `nixos-module`). A VM requires the relation; a standalone member forbids it. `standalone` means no fleet owner, not bare metal: cloud instances and other existing members retain their present meaning. `category` remains organizational and does not infer `kind`. This is an additive contract, not automatic reinterpretation of existing `category = "vm"` entries.

`owner` expresses lifecycle authority more precisely than illustrative `hostMember`; `kind` avoids overloading `platform`. A NixOS microVM under a NixOS owner keeps `platform = "nixos"`, its own name/network facts and independent `build.system/buildPlatform`. No endpoint alias replaces guest identity.

Validate local shape at every member-schema entry point and relations after collision-free fleet assembly. The owner must exist, differ from the guest, be standalone and directly managed. For the first backend, both members have `platform = "nixos"` and consumer-supplied NixOS configurations; the owner enables deploy-rs, the guest has build metadata and the owner exposes the backend's application manifest. Reject missing owners, duplicate ownership declarations, cycles, VM owners, facts-only owners, unsupported backends/pairs and unavailable artifacts before effects. Cross-system pairs are permitted only when the injected runtime module supports the guest system and the graph has suitable build capacity; OS equality alone is not a capability assertion.

Derive sorted `projections.vm.guestsByOwner.<owner> = [ guest-id ... ]` from `members.*.vm.owner`, omitting owners with no guests. There is no separately authored guest list. Future Kubernetes mappings must name node identities explicitly rather than treating member names as node names.

**Alternatives:** only category tags cannot validate authority; using platform would conflate OS and ownership; owner-only opaque guests lose inventory/status; guest SSH creates a competing lifecycle authority. Reject all four.

#### Phase-A authoring surface

The approved Nix contract uses nullable owner-side `host.vmApplication = { backend; manifestVersion; supportedSystems; guests.<guestId>.system; }`, authored by the consumer's runtime module in the owner's configuration. This is a compatibility summary whose guest IDs and systems must exactly agree with the guest relations, not a fleet-level second configuration authority. The application is the existing owner deploy profile/toplevel: the pure `projections.vm.applications.<owner>` view derives IDs, backend/version, systems and `application = ".#deploy.nodes.<owner>.profiles.system.path"`; it accepts no free installable override and forces no artifact. Phase B supplies the readiness/recovery runtime manifest inside that artifact. The pure Nix phase does not implement frontend negotiation or authorize VM deployment.

### 2. Fail closed at transport projection boundaries

VMs remain in the aggregate's members, canonical groups, DNS/DHCP when their real address facts qualify, and documentation. The owner is not substituted into their network records.

VMs never become Ansible SSH hosts (including hostvars/children/guard groups), deploy-rs nodes, standalone static deploy-batch entries or builder routes. Apply this guard independently of enables and through direct library calls, flake outputs, controller-platform variants and consumer hooks. Hooks cannot reinsert an excluded target. Factory defaults must set both guest `deployment.deployRs.enable` and `deployment.ansible.enable` false; any effective true value, including an inherited true, is a validation error naming the guest and owner. This deliberately avoids guessing whether an opt-in was accidental. Inert default connection fields need not fail, but they never authorize guest transport. A VM builder facet that enables advertisement is contradictory and rejected; even malformed input bypassing validation cannot produce a guest route.

Guest `build` describes closure construction only. Advertise guest-backed/emulated execution capacity exclusively through the owner's `host.builder`, with explicit systems/features/lanes and strict owner-endpoint trust. The owner daemon must dispatch that work; no automatic route, duplicate guest advertisement, inferred emulation or SSH trust kind for a VM is introduced. Owner self-exclusion remains correct because its daemon supplies local capacity. Builder-facet implementation is separately owned; this change specifies its VM guard without implementing capacity work.

Canonical group membership remains a member set. Transport inventories and raw deploy batches are filtered views, not the same set for mixed VM groups. Update the previous group-equality claims explicitly in the inventory delta. Direct static batches contain owner entries only if the owner itself meets their normal membership; they do not silently expand a guest-only group. Native selection is the interface that expands ownership.

### 3. Resolve selection to explicit owner units before approval

Resolve canonical selectors and exclusions on member identity once. Then map each selected VM to its owner and deduplicate with directly selected owners. Preserve `selectedMembers` and `requestedGuests` separately from `applicationUnits`. The owner artifact can affect every guest it manages; conservatively report all siblings, distinguishing requested from potentially affected and unchanged guests. Never promise guest-only atomicity. Selecting a guest while excluding its owner or one of its affected siblings is a conflict requiring a revised selection before approval, not silently defeating an exclusion.

Show owner identity, backend, requested guests, all affected siblings, desired artifact references and readiness/recovery limits before the existing mutation approval gate. Approval binds this resolved plan and revision; any later change requires a fresh preview/approval. Headless and MCP paths obey their existing explicit authorization gates with the same expanded plan. A guest-only selection is not skipped for its false deploy-rs enable; an ordinary non-deployable standalone member is still skipped. Refuse unsupported owner adapters before registration/build/mutation, never fall back to guest SSH.

Serialize the full copy/apply/verify/recover transaction per owner, including concurrent runs and direct-owner versus guest selections. Use an owner-enforced lock shared across controllers, not merely a controller-local mutex, with bounded acquisition and a visible busy failure; release only after the transaction/recovery settles. Independent owners may proceed in parallel and do not roll back because another owner failed. Crash recovery must reconcile durable runtime state before admitting a new mutation for that owner.

**Alternative:** silently use the owner's hostname for a guest would hide siblings and misattribute outcomes. Per-guest locks would race a shared host module. Reject both.

### 4. One graph, one build, owner application artifacts

The consumer configuration factory evaluates host and guest configurations in one fleet graph. The owner application artifact contains its activation profile, a versioned typed backend manifest and direct store references to every guest closure/image it may apply. Referencing guest outputs must not create recursive owner/guest configuration dependencies. Manifest entries identify owner, guest, backend, desired immutable guest generation/image and runtime module instance, plus readiness/recovery contract references. Evaluation validates the manifest's IDs against the ownership relation.

The aggregate remains pure JSON data: member IDs, ownership, backend and installable-reference strings only, never derivations or forced realization. Native resolution uses it to build selected owner application artifacts in the existing single batch invocation; referenced guest closures are dependencies, not an extra guest build phase. Necessary sibling dependencies are related builds, not unrelated-member work. Retain the built artifacts and previous recovery references with GC roots through settlement/recovery. A failed guest dependency makes that owner's application ineligible; independent successful owner artifacts may proceed under existing best-effort policy. No copy or activation for an owner whose complete artifact is unverified.

Activation consumes built paths and the manifest, with no guest configuration evaluation, `nixos-rebuild`, image rebuild, flake evaluation or second build on the owner or controller. Runtime restart or selection of a prebuilt image is application, not construction. Static `deployBatch` keeps its cache-warming role; guest dependencies are transitive references of an included owner, never additional guest nodes.

### 5. First backend: nixos-module

Choose exactly one initial backend: `nixos-module`, for a directly managed NixOS owner with NixOS guests whose runtime is controlled by the owner's injected module. No virtualization input is pinned by Mandala; the consumer supplies the module/runtime adapter. The adapter declares supported owner/guest systems and must pass the conformance fixtures. A generic "command succeeded" hook is insufficient.

The module adapter must supply these typed host-side operations over the prebuilt manifest:

| Operation | Required evidence |
| --- | --- |
| inspect | Owner profile/running generation, each affected guest's running generation or immutable image ID, runtime instance identity, readiness and previous runtime/image pointers |
| prepare | Complete desired paths available in the owner store, supported pair, recoverable previous state and a versioned manifest matching ownership |
| apply | Owner module activation and bounded guest restart or prebuilt image selection, with per-guest staged/running disposition |
| verify | Observed running generation/image equals desired identity and declared bounded readiness checks succeed for each affected guest |
| restore | Restore previous owner state as needed and each affected guest's previous runtime/image, then verify those identities and readiness |

All operations execute through the owner transport with its flattened connection/privilege settings; no guest endpoint is dialled. Readiness may use owner-local runtime probes or explicitly declared service probes, never guest SSH. The module must expose the actually running guest identity, not merely the desired config, image symlink or host profile. Immutable image identity or running guest generation must be unambiguous across restarts. Missing observations, unsupported verification or absent recoverable previous runtime fail closed before mutation. Readiness deadlines and diagnostics are manifest contract data, bounded and tested.

"Deployed"/guest `confirmed` means requested runtime identity observed plus readiness; owner deploy-rs confirmation alone does not prove this. A staged image or boot-only owner update leaves guest `pending`, not deployed. Pending is an explicitly successful staging disposition, counted separately from verified deployment, consistent with existing reboot-pending host behavior. If an expected live restart never completes, report failure, not indefinite pending. Dry activation may build/copy but cannot mutate host profiles, guest runtime, image pointers or recovery state; report preview/not-applied rather than deployed.

If the target's live-switch preflight refuses, preserve transactional boot-only host staging and guest pending status. Never restart guests to circumvent the inhibitor. Neither explicit boot intent nor a host confirmation upgrades a guest to verified without observation.

**Alternatives:** start with a generic multi-backend abstraction or guest-exec. Rejected to keep the first conformance/recovery boundary finite. The owner does not execute a foreign guest's switch binary as if it were a host binary.

### 6. Recovery is runtime restoration, not profile reversal

Before mutation, atomically persist and root the previous owner profile/boot default and affected guests' runtime generation/image pointers, manifest version, readiness contract and data-rollback limitations. The first backend requires recoverable prior runtimes; first-time creation without one is refused rather than claiming fictitious rollback support.

If module activation or verification fails, recover within that owner according to its existing auto-rollback policy, extending host recovery with guest runtime restoration. Record primary and recovery failures independently. With auto-rollback disabled, leave an explicit failed/unrecovered result and recovery information, not rolled-back. Host magic rollback may restore a profile but is only one input: guest restore/verification still must run. An owner with VM guests is rolled-back only when previous affected owner state and every affected previous guest runtime plus readiness are verified. Do not undo independent successful owners.

A module can restore one sibling and fail another. Preserve each observed outcome, mark the owner unit partial-recovery/failed and the run nonzero; never collapse that to rolled-back. Persist unknown when a crash/timeout prevents observation, rather than infer success from last intent. On interruption obey existing no-new-effects/process-ownership guarantees; leave interrupted/recovery-required metadata if bounded recovery cannot complete, with explicit reconciliation before another run. No detached recovery or automatic replay of unverified commands.

Writable guest disks, application data and external services are not reverted by a generation/image rollback. Report this limitation in the preview and recovery record; do not use a successful runtime restoration as proof of data restoration.

### 7. Versioned contracts and observable outcomes

A VM-bearing aggregate uses `schemaVersion = 2`, with pure ownership/index/application-reference projections. Non-VM fleets may retain the current version-1 shape and bytes. VM-aware readers accept both versions; older readers must reject version 2 before deployment, not ignore ownership and treat guests as targets.

Add an owner-unit VM section to durable deploy metadata, keyed by owner and containing selected members, requested/affected guests, backend/manifest version, desired/previous/observed immutable identities, readiness results, per-guest outcomes and recovery diagnostics. Keep the owner as the existing host event identity; guest records carry explicit `owner` and `guest` IDs in an additive VM event/metadata envelope, not invented SSH hosts. Updated readers compute owner success from both host and guest evidence. Unknown fields/events remain ignorable under the existing format protocol; old readers' host-only confirmation is not VM deployment evidence. Preserve all existing failed/rolled-back precedence, with partial/unknown recovery also unsuccessful. Do not repurpose existing `confirmed` to mean "staged".

Pin exact serialization and reader judgements before shipping. CLI, TUI and MCP use the same resolved plan, persisted observations and settlement; an attached observer must reconstruct outcomes after restart without contacting/re-evaluating the fleet.

## Delivery and Verification

Add schema and pure fixtures before orchestration; keep ordinary non-VM output parity. Then implement guarded projections/owner resolution, graph/artifacts, the single module adapter, recovery and durable frontend observation in separate shippable increments. Do not switch a consumer as part of these increments.

Use fake fleets (acme/globex) and injected build/transport/runtime effects only. Assert exact effect traces, no guest SSH or activation-time evaluation, build dependency failure isolation, concurrent-run locking, stale recovery reconciliation, generation/readiness mismatches, staged outcomes, host-only rollback, sibling partial recovery, missing evidence, timeouts and cancellation. Serialization tests cover old records and unsupported aggregate rejection. Existing checks/target names are owned by `flake.nix`; planned new check names are listed in `tasks.md`.

## Approval and Open Items

The first-class identity/owner-only model and initial module-managed scope are fixed inputs. The field names, backend ABI, version-2 aggregate and recovery details above are proposed plan decisions requiring review before implementation.

**Explicit open item, deferred:** a macOS owner needs a separately designed host-application adapter, including artifact construction, runtime inspection and recovery. Today's deploy-node projection accepts NixOS configurations only. This plan neither adds Darwin nodes nor treats a facts-only macOS member as directly deployable. Support for a Linux builder VM under a macOS owner is blocked until that adapter is designed and approved; no guest SSH workaround.

No other blocking scope question remains for these planning artifacts. Artifact completeness is not approval or authorization to implement or deploy.
