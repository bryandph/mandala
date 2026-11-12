## Planning gate
- [ ] Review and approve this plan after `vm-member-host-application`; artifact completeness is not implementation approval.
- [ ] Approve darwinConfigurations enrollment, darwin-system/darwin-module ABI, boot refusal, prior-wrapper adoption prerequisite and retained-runtime recovery policy.

## Additive schema and pure graph
- [ ] Add collision-checked Darwin configuration discovery and supported owner/guest pair validation; preserve unenrolled facts-only and NixOS behavior.
- [ ] Add typed artifact/manifest capability and wrapper/system path mapping in version-2 pure aggregate; reject unknown adapters before effects.
- [ ] Add acme/globex fake-fleet fixtures and planned `darwin-owner-schema` / `darwin-owner-projection` checks; prove guest transport/builder exclusion and owner-only capacity.
- [ ] Extend deploy nodes/batches and controller variants additively; assert one batch build with immutable guest dependencies and no target construction.

## Owner application and recovery
- [ ] Add bounded Darwin profile adapter using deploy-rs custom activation, explicit system profile/root privilege and compatible prior-wrapper validation.
- [ ] Inject typed owner manager operations; verify active runtime image/lineage/disk and readiness before canary confirmation.
- [ ] Enforce cross-controller owner locks, approved sibling impact, durable recovery prerequisites and prior-runtime/disk retention before mutation.
- [ ] Add restore/reconcile and truthful failed/partial/unknown outcomes; preserve cancellation/process ownership and independent-owner isolation.
- [ ] Test supported dry preview, boot/unsafe self-deployment rejection, staged guests and no guest SSH/exec or activation-time evaluation/build.
- [ ] Add `darwin-owner-controller` / `darwin-owner-state` checks and native injected-effect tests covering Darwin/Linux controllers, self-deploy SSH, stale identity, disk/key regeneration, host-only rollback and restart recovery.
- [ ] Pin canonical bytes and reader judgements across CLI/TUI/MCP; old records and unknown event fields remain compatible.

## Verification and qualification
- [ ] Run `nix fmt -- --check .` and `nix flake check`.
- [ ] Run existing `nix build .#checks.aarch64-darwin.fake-fleet .#checks.aarch64-darwin.builder-projection .#checks.aarch64-darwin.builder-adapters` and the controller-platform expression exercised by fake-fleet; repeat applicable checks on Linux.
- [ ] Run planned `nix build .#checks.aarch64-darwin.darwin-owner-schema .#checks.aarch64-darwin.darwin-owner-projection .#checks.aarch64-darwin.darwin-owner-controller .#checks.aarch64-darwin.darwin-owner-state` after registering those new names; they do not exist yet.
- [ ] Run `cargo test -p mandala-deploy -p mandala-core` with isolated fake programs and explicit failure injection.
- [ ] Review public neutrality, scope-only file list and no consumer changes.
- [ ] Obtain separate approval for pinned nix-darwin/deploy-rs/VM-module runtime qualification; test Darwin watcher confirmation, connection loss, retained-runtime restore and daemon dispatch before claiming live support.
- [ ] Keep implementation, consumer migration and live rollout separate; do not switch a consumer in this change.
