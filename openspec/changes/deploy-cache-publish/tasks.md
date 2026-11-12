## 1. Approval and additive option policy

- [ ] 1.1 Obtain explicit plan approval before implementation; confirm this change's edge-case choices without reopening the fixed interface, advisory failure policy or trust boundary.
- [ ] 1.2 Add an additive auto/enable/disable option and a pure resolver taking explicit environment inputs. Test URL-only enablement, absent/empty values, netrc-only disablement, explicit enable without URL, invalid URL, disable ignoring invalid configuration, and conflicting flags in either order.
- [ ] 1.3 Add pure argv construction and safe diagnostic types. Pin stable path deduplication/order, one upload argv, optional `--option netrc-file`, no shell interpolation, and no flake installables, new build, or upload signature relaxation.

## 2. Shared publish effect and fallback

- [ ] 2.1 Add an effect-isolated publish helper with an injectable Nix executable and owned process-group capture, using `crates/mandala-deploy/src/process.rs` rather than introducing another subprocess lifecycle.
- [ ] 2.2 Insert the phase after `build_profiles_with` and any `recover_profiles` result, before all `fan_out_with` work. Preserve profile GC roots and sticky build-failed hosts. Test a barrier-controlled stub trace proving one batch build, effect-free recovery, one union upload and then host fan-out, without another evaluation/build.
- [ ] 2.3 Test shared profiles, pre-existing cache paths handled by the same command, partial build success, no successful paths, fatal resolution errors, interrupted builds and halt-on-build-failure. Assert no upload for ineligible paths or aborted builds.
- [ ] 2.4 Implement warn-and-fallback for spawn/nonzero upload failures without retry or run failure from upload alone. Test subsequent per-host SSH copy, success without cache reachability, partial upload failure, and unchanged destination substitution / `fastConnection` behavior.
- [ ] 2.5 Add real-binary, shell-stub termination coverage to `crates/mandala/tests/termination.rs`: SIGINT/SIGTERM during publish, descendant cleanup and escalation, no new effects after termination, unreaped guard cleanup, and no signalling of reaped PGIDs. Preserve the documented normally-reaped-leader limitations.

## 3. Credentials and durable observation

- [ ] 3.1 Keep credentials in memory only for the upload command; never read a netrc or secret store. Remove the upload hint from unrelated child environments and mask it in diagnostic argv. Test paths with spaces and ensure exact argv boundaries.
- [ ] 3.2 Redact URL userinfo/query/fragment and netrc paths in every persistent and visible surface, including invalid-input and spawn errors. Use stubs that print sensitive values to stdout/stderr; prove raw output is discarded and safe reason/process status persists.
- [ ] 3.3 Add the shared `publish.jsonl` stream, atomic additive metadata and non-terminal eligible-host milestones. Pin outcomes, path counts, safe reasons, legacy absence, additive-field tolerance, unchanged host counts and sticky result precedence in serialization/reader tests.
- [ ] 3.4 Update shared status projections and CLI/TUI/MCP observers for the active cache-publish phase, redacted destination and advisory outcome. Test attaching mid-publish and reading a settled failed-publish/successful-host run; never convert the upload rc into a host or run rc.

## 4. Launch parity and mode fidelity

- [ ] 4.1 Wire `--cache-push` / `--no-cache-push` through headless and standalone TUI commands, preserve the tri-state in explorer and typed MCP launch paths, and retain existing confirmation/wrapper gates. Test omission/enable/disable parity against the same engine resolver.
- [ ] 4.2 Test publishing during dry activation, run-level boot and mixed per-member modes. Assert no activation in dry mode, reboot-pending remains distinct from confirmed, and internal resolution/plan-only paths never upload.
- [ ] 4.3 Document the opt-in environment and overrides, netrc process boundary, redacted diagnostics, direct-copy fallback, existing trust prerequisites and static cache-warming non-interference. Update `docs/deploy-termination.md` and align the shared fleet-cli/state-format specs with the reviewed additive contracts when implementation ships. Use fictional examples only.

## 5. Verification and review

- [ ] 5.1 Run targeted workspace tests for the deploy resolver/argv/phase, registry serialization, frontend projections and real-binary termination using stubs; include existing deployment/rollback regression tests. Run `nix develop -c cargo test --workspace` for full workspace coverage.
- [ ] 5.2 Run `nix build .#checks.<system>.fake-fleet` and `nix build .#packages.<system>.mandala-rs` (substitute the actual system; target names live in `flake.nix`). The package gate runs workspace tests and termination integrations. No live cache or fleet may be needed.
- [ ] 5.3 Run `nix fmt -- --check .` and `nix flake check`. Record exact results and environmental limitations; do not treat unevaluated platforms or unrun tests as passed.
- [ ] 5.4 Run `openspec validate deploy-cache-publish --strict` when the OpenSpec CLI is available and the organization-neutral content check over new artifacts. Review requirement/scenario coverage and report any unavailable validation tool explicitly.
- [ ] 5.5 Return implementation evidence for review; do not merge, release, mutate a fleet/cache, or change a consumer credential wrapper without its separate authorization.
