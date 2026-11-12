## 1. Approval and additive option policy

- [x] 1.1 Obtain explicit plan approval before implementation; confirm this change's edge-case choices without reopening the fixed interface, advisory failure policy or trust boundary.
- [x] 1.2 Add an additive auto/enable/disable option and a pure resolver taking explicit environment inputs. Test URL-only enablement, absent/empty values, netrc-only disablement, explicit enable without URL, invalid URL, disable ignoring invalid configuration, and conflicting flags in either order.
- [x] 1.3 Add pure argv construction and safe diagnostic types. Pin stable path deduplication/order, one upload argv, optional `--option netrc-file`, no shell interpolation, and no flake installables, new build, or upload signature relaxation.

## 2. Shared publish effect and fallback

- [x] 2.1 Add an effect-isolated publish helper with an injectable Nix executable and owned process-group capture, using `crates/mandala-deploy/src/process.rs` rather than introducing another subprocess lifecycle.
- [x] 2.2 Insert the phase after `build_profiles_with` and any `recover_profiles` result, before all `fan_out_with` work. Preserve profile GC roots and sticky build-failed hosts. Test a barrier-controlled stub trace proving one batch build, effect-free recovery, one union upload and then host fan-out, without another evaluation/build.
- [x] 2.3 Test shared profiles, pre-existing cache paths handled by the same command, partial build success, no successful paths, fatal resolution errors, interrupted builds and halt-on-build-failure. Assert no upload for ineligible paths or aborted builds.
- [x] 2.4 Implement warn-and-fallback for spawn/nonzero upload failures without retry or run failure from upload alone. Test subsequent per-host SSH copy, success without cache reachability, partial upload failure, and unchanged destination substitution / `fastConnection` behavior.
- [x] 2.5 Add real-binary, shell-stub termination coverage to `crates/mandala/tests/termination.rs`: SIGINT/SIGTERM during publish, descendant cleanup and escalation, no new effects after termination, unreaped guard cleanup, and no signalling of reaped PGIDs. Preserve the documented normally-reaped-leader limitations.

## 3. Credentials and durable observation

- [x] 3.1 Keep credentials in memory only for the upload command; never read a netrc or secret store. Remove the upload hint from unrelated child environments and mask it in diagnostic argv. Test paths with spaces and ensure exact argv boundaries.
- [x] 3.2 Redact URL userinfo/query/fragment and netrc paths in every persistent and visible surface, including invalid-input and spawn errors. Use stubs that print sensitive values to stdout/stderr; prove raw output is discarded and safe reason/process status persists.
- [x] 3.3 Add the shared `publish.jsonl` stream, atomic additive metadata and non-terminal eligible-host milestones. Pin outcomes, path counts, safe reasons, legacy absence, additive-field tolerance, unchanged host counts and sticky result precedence in serialization/reader tests.
- [x] 3.4 Update shared status projections and CLI/TUI/MCP observers for the active cache-publish phase, redacted destination and advisory outcome. Test attaching mid-publish and reading a settled failed-publish/successful-host run; never convert the upload rc into a host or run rc.

## 4. Launch parity and mode fidelity

- [x] 4.1 Wire `--cache-push` / `--no-cache-push` through headless and standalone TUI commands, preserve the tri-state in explorer and typed MCP launch paths, and retain existing confirmation/wrapper gates. Test omission/enable/disable parity against the same engine resolver.
- [x] 4.2 Test publishing during dry activation, run-level boot and mixed per-member modes. Assert no activation in dry mode, reboot-pending remains distinct from confirmed, and internal resolution/plan-only paths never upload.
- [x] 4.3 Document the opt-in environment and overrides, netrc process boundary, redacted diagnostics, direct-copy fallback, existing trust prerequisites and static cache-warming non-interference. Update `docs/deploy-termination.md` and add fleet-cli/state-format delta specs under this change. Canonical synchronization happens at archive in a separate step after merge; do not edit `openspec/specs/` directly. Use fictional examples only.

## 5. Verification and review

- [x] 5.1 Run targeted workspace tests for the deploy resolver/argv/phase, registry serialization, frontend projections and real-binary termination using stubs; include existing deployment/rollback regression tests. Run `nix develop -c cargo test --workspace` for full workspace coverage.
- [x] 5.2 Run `nix build .#checks.<system>.fake-fleet` and `nix build .#packages.<system>.mandala-rs` (substitute the actual system; target names live in `flake.nix`). The package gate runs workspace tests and termination integrations. No live cache or fleet may be needed.
- [x] 5.3 Run `nix fmt -- --check .` and `nix flake check`. Record exact results and environmental limitations; do not treat unevaluated platforms or unrun tests as passed.
- [x] 5.4 Run `openspec validate deploy-cache-publish --strict` when the OpenSpec CLI is available and the organization-neutral content check over new artifacts. Review requirement/scenario coverage and report any unavailable validation tool explicitly.
- [x] 5.5 Return implementation evidence for review; do not merge, release, mutate a fleet/cache, or change a consumer credential wrapper without its separate authorization.

## Verification notes

- The plan and edge choices were explicitly approved before implementation. No live cache, secret store, consumer repository, or fleet process was used by the implementation tests.
- Task 4.3 is fulfilled by the fleet-cli and fleet-state-formats deltas inside this change. Canonical synchronization is deferred to the separate archive step after merge; this change is not archived here.
- After rebasing onto the merged main branch, `nix develop -c cargo test --workspace` passed: 449 passed, 0 failed, 12 ignored across 40 suites. Scoped clippy for mandala-core, mandala-deploy, mandala, mandala-tui and mandala-mcp passed with all targets, no dependencies and warnings denied.
- All executable steps in `.github/workflows/ci.yml` passed locally: synthetic controller identity evaluation, root flake check, format check, template flake check, showcase flake check, showcase network-tf build, and mandala-rs package build. Explicit aarch64-darwin fake-fleet and mandala-rs package targets also passed. These are local aarch64-darwin results; incompatible platforms and Linux-only VM execution are not claimed.
- The OpenSpec CLI was unavailable on PATH, so strict CLI validation was not run. Local structural checks covered all seven artifacts, 13 requirements and 48 WHEN/THEN scenarios across the three delta capabilities.
- The organization-neutral grep found only pre-existing lines in touched files; no matching line was newly introduced. Baseline clippy cleanups were isolated from the feature in their own commit.
