## 1. Review and additive protocol

- [x] 1.1 Record the settled decisions: refuse new mutating continuations on source change and require a new campaign; protect open campaigns' targets until close; create-and-deploy in one step with normal real-mutation confirmation and an explicit dry creation preview. These choices are approved; implementation remains separately gated and is not authorized by this docs change.
- [x] 1.2 Add version-1 campaign target/manifest types, confined identifiers and atomic sorted serialization without altering existing run/event versions. Add legacy/new/future/corrupt golden fixtures and reader judgments.
- [ ] 1.3 Add interprocess locking, generation-checked exclusion/lifecycle transitions and prepared-attempt reconciliation. Test concurrent dispatch/update/close, unknown ownership and restart without duplicate effects.
- [ ] 1.4 Extend retention additively to protect open campaigns, linked runs and GC roots; prune closed groups only when inactive. Test count/age limits, live supervisors, partial allocation and missing manifests; prohibit mixed old retention writers while campaigns are enabled.

## 2. Immutable capture and execution policy

- [ ] 2.1 Capture effective Nix source content, root dirty state, recursive resolved inputs and submodule actual/gitlink/dirty identities into rooted snapshots. Refuse uncaptured impurity and inconsistent capture; test equal revisions with distinct dirty contents.
- [ ] 2.2 Resolve frozen inventory membership, reasoned selector exclusions, settings, profile derivation/output and system-toplevel paths before binding. Test zero eligible members, non-deploy capability skips, unknown outputs and profile/system correspondence.
- [ ] 2.3 Add create-and-deploy first-attempt dispatch after durable binding/linkage, plus failed/pending retry selection and exact-path realization using recorded identities. Test first-attempt confirmation refusal and dry creation preview binding without an attempt or host effects. Preserve successful outcomes and historical run failures, best-effort recovery, per-host rollback, termination and existing publish order; dry previews leave real work pending.
- [ ] 2.4 Barrier-test source movement during capture and after binding, nested local-input content changes and moved recursive submodule pointers without root revision movement. Assert no effects on failed capture, no live-source reevaluation on retry, and explicit new-campaign refusal on source mismatch/unavailability.
- [ ] 2.5 Test partial build/activation failures and retry: only failed/pending eligible members execute; confirmed and reboot-pending siblings receive no copy or activation; missing rooted outputs never trigger a replacement target.

## 3. Exclusions, reboot and verification

- [ ] 3.1 Share effective-exclusion projection across every campaign deploy/retry/reboot/verification path. Test add/remove/reason updates, stale generation refusal, unknown names, no widening, successful-member unskip and mutation-in-flight refusal.
- [ ] 3.2 Dispatch reboot through the existing fleet app with exact eligible reboot-pending names, fresh installed-profile proof, serial/drain controls and normal confirmation. Never infer verification from command exit alone.
- [ ] 3.3 Add read-only bound-target verification with snapshot freshness and boot-completion requirements. Preserve excluded rows/reasons outside eligible denominators; close records incomplete results without asserting success.

## 4. Drift and additive frontend integration

- [ ] 4.1 Add shared expectation-unknown judgment and independent campaign/latest-source comparison projections. Test missing/corrupt/stale/dirty-ambiguous/identity-incomplete caches, partial evaluation, unsupported campaign schema and no expectation ever reported clean.
- [ ] 4.2 Test staged profile versus current, boot-critical differences, missing boot facts, profile-wrapper/system-toplevel distinction and latest-source movement without campaign mutation.
- [ ] 4.3 Add CLI lifecycle/JSON and TUI campaign list/detail/dialogs with run links. Keep existing deploy commands, runs/attach and unqualified tool fields compatible; test create-and-deploy, creation preview, exact names, reasons, source movement and explicit action review.
- [ ] 4.4 Add MCP lifecycle tools and optional campaign drift view with shared confirmation/generation checks, confined identifiers, paginated detail and audit. Test old tool-call replay, real create-and-deploy, dry creation preview without an attempt, unchanged retry dry default, creation/retry required_confirm refusals and bounded run status.
- [ ] 4.5 Route context followers and standalone CLI through the same policy and durable locks. Test reload isolation, leadership loss, source-keyed evaluator behavior and no automatic mutation replay.

## 5. Verification and delivery gates

- [ ] 5.1 Run effect-isolated Rust unit/integration tests with temporary Git repositories, fake source graphs and stub Nix/SSH/survey/reboot executables, never a live fleet. Gate serialization bytes and eligibility/projection judgments across CLI/TUI/MCP.
- [ ] 5.2 Run `nix build .#checks.<system>.fake-fleet` and `nix build .#packages.<system>.mandala-rs` for supported systems from `flake.nix`; retain source-race and partial-failure traces proving no unnecessary reactivation.
- [ ] 5.3 Run `nix fmt -- --check .` and `nix flake check`; check organization-neutral fixtures and review all OpenSpec deltas before any implementation merge.
- [ ] 5.4 Document rollout/upgrade ordering for retention writers and frontend compatibility; obtain separate approval before switching consumers. Synchronize canonical specs only at archive.
