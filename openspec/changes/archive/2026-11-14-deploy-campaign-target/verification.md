# Implementation verification

Validated implementation revision: `39ee4f8f294fa4c5f1b277cbf091aabdd3e677a5`.
This record adds no consumer switch, live operation or canonical-spec sync.

## Gates

- `cargo test --workspace`: **542 passed, 0 failed, 19 ignored**. The ignored
  cases are generated tool-documentation examples. Two child-process lock probes
  are not counted twice in the workspace total.
- `cargo clippy -p mandala-core -p mandala-mcp -p mandala-tui -p mandala --all-targets --no-deps -- -D warnings`: passed.
- `cargo fmt --all` and `git diff --check`: passed.
- Organization-neutrality scan over all changed files: no matches.

Every executable step from `.github/workflows/ci.yml` was run locally:

| Step | Result |
| --- | --- |
| Synthetic controller identity `nix eval`, using the pinned local Git source reference from the workflow | Passed |
| `nix flake check` | Passed on the native system; incompatible systems explicitly omitted by Nix |
| `nix fmt -- --check .` | Passed |
| `nix flake check ./templates/fleet --override-input mandala . --no-write-lock-file` | Passed |
| `nix flake check ./examples/showcase --override-input mandala . --no-write-lock-file` | Passed |
| `nix build ./examples/showcase#network-tf --override-input mandala . --no-write-lock-file` | Passed |
| `nix build .#mandala-rs` | Passed on rerun; see the timing-test disclosure below |

The local checkout and installed Nix replace the workflow's checkout/installer
setup actions. In addition, both `checks.<system>.fake-fleet` and
`packages.<system>.mandala-rs` were built successfully for **all four** systems
listed in `flake.nix`: `aarch64-darwin`, `aarch64-linux`, `x86_64-darwin` and
`x86_64-linux`. `nix flake check --all-systems --no-build` also passed. This does
not claim that every cross-platform VM check was built.

An initial native package build hit the pre-existing timing-sensitive
`dropping_frontend_leaves_supervised_native_child_alive` assertion in the
unchanged TUI deploy-flow tests. Rebuilding the same derivation in the
all-systems build succeeded, and the exact workflow package command then passed
on rerun. No test was weakened or disabled.

## Metadata equivalence probes

Temporary local source fixtures, never a live fleet, exercised the real Nix
2.34.x capture path. Their derivation embeds all root/input `sourceInfo` fields,
original source paths and flake paths. Full capture, including the always-on
live/frozen profile and system drvPath guard, passed for:

- Dirty Git root with a clean Git input, and dirty Git root with a dirty input.
- Clean Git root and input, including a committed lock-file form.
- A root flake subdirectory with relative flake and non-flake inputs.
- Newly locked and kept-relative-input forms, and a relative follows case.

The original dirty-revision reproduction changed from an incorrect `missing`
value under bare path evaluation to the original dirty revision under capsule
replay. The persisted fixture is
`crates/mandala-core/tests/fixtures/campaign-metadata/`; its builder cannot
succeed and its destination is reserved `.invalid`. Rust command fixtures also
strip profile or system metadata deliberately and require refusal before
binding. No derivation builder, copy, activation or reboot was run by the real
metadata probes.

## Added tests and their properties

There are 84 new authored test cases, plus an updated registration-order
assertion. Source tests replace Nix/Git/SSH/survey/reboot effects or use isolated
local process fixtures; they do not operate a fleet.

### Core protocol — `crates/mandala-core/src/campaign/tests.rs`

- `canonical_manifest_has_sorted_objects_no_newline_and_golden_bytes`: pins version-1 bytes, round-trip data and digest behavior.
- `capability_skips_cannot_be_unskipped_and_all_excluded_is_no_run`: capability notices are not operator skips; empty eligibility allocates nothing.
- `close_retains_incomplete_outcomes_and_prevents_future_mutations`: administrative closure is not deployment success.
- `closed_campaign_prunes_as_unit_only_after_close_age`: retention age begins at close and removes the inactive group together.
- `corrupt_missing_or_tampered_manifest_never_synthesizes_a_target`: invalid persistence cannot invent expectations.
- `cross_process_lock_probe`: helper test endpoint used by the independent-process lock assertion.
- `dry_retry_does_not_turn_preview_into_real_success`: preview outcomes leave real work pending.
- `exclusions_are_durable_generation_checked_and_never_erase_success`: reasoned policy history survives updates without clearing prior success.
- `future_schema_is_identifiable_but_never_mutable`: future versions are inspectable only at identification level.
- `generated_id_confinement_rejects_traversal_and_wrong_shapes`: untrusted identifiers cannot escape their namespace.
- `interrupted_unlinked_allocations_share_closed_retention_and_live_protection`: interrupted allocations stay in the retention unit, including live protection.
- `live_linked_supervisor_prevents_closed_unit_pruning`: close cannot unroot a surviving supervised allocation.
- `missing_run_metadata_cannot_prove_a_closed_unit_is_inactive`: missing evidence fails retention closed.
- `open_campaign_retains_manifest_roots_and_linked_runs_past_age_and_count`: open targets survive ordinary pruning between attempts.
- `os_lock_serializes_independent_processes_not_only_frontend_threads`: two actual processes cannot own the same campaign lock.
- `prepared_or_running_ownership_blocks_retry_close_and_policy_update`: unresolved ownership blocks competing mutation.
- `preview_binding_is_inert_and_creates_no_attempt_or_run_directory`: bind/show preview has no implicit execution.
- `real_dispatch_requires_exact_confirmation_and_rejects_source_movement`: names and source identity are checked before preparation.
- `recursive_submodule_checkout_movement_changes_source_identity`: actual submodule revision movement is not hidden by a root label.
- `retry_preserves_confirmed_and_staged_success_with_provenance`: retries retain successful siblings and producing-run references.
- `selector_can_only_narrow_the_bound_membership`: optional selection cannot add hosts.
- `shared_lock_excludes_other_frontends_and_releases_on_drop`: frontend exclusion and lock release use durable OS ownership.
- `source_identity_uses_dirty_content_and_recursive_inputs_not_revision_labels`: labels alone cannot establish equality.
- `stale_generation_refuses_after_another_frontend_commits`: concurrent review cannot silently adopt a new generation.
- `symlinked_campaign_directories_and_manifests_are_refused`: filesystem aliases cannot redirect campaign access.
- `target_validation_refuses_unknown_outputs_and_invalid_members`: incomplete targets cannot be bound.
- `uncertain_or_incomplete_evidence_never_settles_an_attempt`: absent terminal proof cannot grant retry or success.
- `unknown_fields_are_ignored_and_failed_transactions_leave_original_bytes`: forward-compatible reads and failed writes preserve the original state.

### Source capture — `crates/mandala-core/src/campaign/source/tests.rs`

- `all_excluded_and_non_deploy_only_capture_do_not_build_or_launch`: empty eligible work has no build or run dispatch.
- `campaign_reboot_links_before_effects_preserves_wrapper_options_and_does_not_verify`: reboot ownership is durable before effects; wrapper/order/drain survive and exit zero is not boot proof.
- `capture_refuses_root_or_nested_source_movement_without_binding`: capture races publish no ready target.
- `chained_follows_and_shared_nodes_have_full_content_identities`: input aliases resolve to their full graph identities.
- `concurrent_capture_directories_never_share_root_ownership`: concurrent capture cannot reuse or delete another capture's roots.
- `create_and_deploy_retry_preserves_success_and_refuses_moved_or_unavailable_source`: fake execution covers durable first dispatch, publish ordering, partial failures, exact retry, successful-sibling preservation and no-op behavior.
- `creation_confirmation_and_explicit_preview_have_no_implicit_runs`: refusal and dry creation do not start work.
- `dirty_revision_labels_preserve_the_available_revision_without_claiming_content_equality`: available revision provenance is retained without replacing content identity.
- `durable_source_references_reject_inline_credentials_and_opaque_queries`: unsafe references cannot enter durable provenance.
- `full_capture_freezes_local_lock_nodes_and_never_evaluates_live_inventory`: membership/settings come from the frozen graph, not a warm/live inventory.
- `incomplete_archives_and_cyclic_follows_are_refused`: unresolved graphs cannot become targets.
- `metadata_faithful_capsule_passes_and_stripped_profile_or_system_metadata_refuses`: both derivation identities are always checked against live pure evaluation.
- `relative_inputs_inherit_archived_parent_metadata_and_unsupported_nix_refuses`: relative source ownership is explicit and unsupported adapters fail closed.
- `source_identity_includes_fetcher_metadata_not_capsule_implementation_bytes`: influencing metadata changes source identity; mechanical artifact changes do not.
- `source_movement_after_target_evaluation_is_a_binding_barrier`: a final source check protects the publish boundary.
- `source_observation_does_not_evaluate_inventory_or_resolve_profiles`: continuation identity checks do not replace captured host targets.
- `submodule_capture_records_gitlink_actual_revision_dirty_state_and_content`: original/actual module identity and captured content are distinct facts.
- `unknown_profile_or_system_path_prevents_binding_and_host_effects`: unresolved intended outputs are not filled from another source.
- `wrapper_and_system_expectations_are_distinct_and_correspondence_is_required`: wrapper paths cannot be mistaken for system-toplevel expectations.

### Execution — `crates/mandala-core/src/campaign/execution/tests.rs`

- `a_controller_exit_without_complete_host_evidence_remains_uncertain`: a zero exit alone does not settle hosts.
- `additive_run_metadata_pins_attempt_digest_generation_and_identity`: ordinary runs retain confined campaign references without replacing legacy fields.
- `dead_unlinked_preparation_reconciles_to_pending_without_a_new_run`: interrupted preparation is reconciled without relaunch.
- `failed_root_initialization_never_publishes_a_ready_manifest`: rooting is completed before manifest publication.
- `linked_reconciliation_uses_terminal_host_evidence_and_preserves_run_failure`: aggregate recovery never rewrites historical run failure.
- `live_prepared_owner_cannot_be_reconciled_or_replayed`: a live owner remains authoritative.
- `reconstruction_uses_captured_activation_and_settings_not_latest_inventory`: dispatch consumes recorded execution settings.

### Shared views and verification — `crates/mandala-core/src/campaign/api/tests.rs`

- `bound_drift_survives_missing_checkout_and_unproven_latest_cache_is_unknown`: the two expectation bases remain independent.
- `close_racing_survey_preserves_closed_manifest_and_observable_ordinary_run`: a barrier proves report retention and no closed-manifest rewrite, including inode identity.
- `closed_verification_is_comparison_only_without_survey_attempt_or_manifest_write`: closed reads cannot create lifecycle work.
- `detail_pages_targets_attempts_sources_and_verification_with_totals`: detailed data is paginated with explicit totals/continuations.
- `legacy_cache_forms_are_never_campaign_identity_and_future_schema_has_no_clean_judgment`: old cache data and unsupported manifests cannot prove success.
- `list_and_inspect_are_read_only_and_do_not_create_missing_roots`: discovery does not implicitly enable campaigns.
- `missing_boot_and_staged_profile_are_incomplete_but_exclusions_are_explicit`: final proof and eligible denominators remain conservative.
- `missing_expectations_and_incomplete_entry_sets_never_mean_complete`: partial or duplicate rows cannot establish complete verification.
- `open_verification_registers_before_survey_and_records_generation_bound_evidence`: observation ownership and provenance are durable before survey.
- `policy_change_during_survey_never_asserts_completion_for_new_denominators`: stale evidence cannot verify a changed eligible set.
- `policy_refusals_leave_bytes_unchanged_and_narrowing_uses_bound_members`: confirmation/generation failures have no policy effects.
- `reboot_selection_uses_fresh_installed_proof_and_drift_never_grants_retry_permission`: observed drift does not revoke historical success into redeployment permission.
- `verification_tracks_the_survey_child_pid_until_it_is_reaped`: a long-lived frontend is not mistaken for a live survey after completion.

### CLI and native compatibility

In `crates/mandala-core/src/campaign/cli.rs`:

- `create_is_real_by_default_and_preview_is_an_explicit_no_effect_path`: CLI start semantics and preview are distinct.
- `reboot_keeps_existing_drain_default_with_explicit_disable_and_policy_requires_reason`: safety defaults and reason requirements survive parsing.
- `retry_requires_generation_and_preserves_dry_mode_and_exact_confirmation`: typed intent is not broadened by argument handling.

In `crates/mandala-core/src/engines/deploy.rs`:

- `campaign_build_rejects_a_link_not_matching_the_bound_output`: captured output mapping is revalidated.
- `campaign_build_uses_recorded_derivations_without_any_resolution`: retry never evaluates a fresh installable.
- `campaign_partial_build_recovers_exact_outputs_without_retargeting`: only verified successful builds proceed to host effects.
- `legacy_build_keeps_its_existing_out_link_mapping_semantics`: the stronger check is campaign-only.

In `crates/mandala-core/src/registry.rs`:

- `campaign_links_fail_closed_without_changing_legacy_retention`: unavailable campaign data does not reclassify linked runs as legacy.
- `manifest_link_protects_a_run_before_additive_metadata_is_published`: interrupted two-sided linking retains evidence.

In `crates/mandala-core/src/runner.rs`:

- `controller_publish_and_member_mirror_preserve_legacy_bytes`: the shared controller encoder preserves publish milestone, diagnostic and member-mirror shapes.

### MCP and TUI

- `real_creation_starts_in_the_same_call_but_preview_refusal_and_empty_work_do_not` (`mandala-mcp/src/effects/campaign.rs`): supervised first execution is conditional on successful real review, never preview/refusal/reconciliation.
- Updated `legacy_prefix_and_additive_campaign_tools_in_registration_order` (`mandala-mcp/src/tools.rs`): the legacy surface remains the registration prefix.
- `campaign_tools_preserve_typed_intent_audit_and_no_replay_without_warm_inventory` (`mandala-mcp/tests/campaigns.rs`): new tools retain typed names/generations/options, audit origin and no-replay discipline without using latest inventory.

In `crates/mandala-tui/src/app/campaign/tests.rs`:

- `cancelling_review_never_dispatches_or_creates_a_background_request`: cancellation is effect-free.
- `reason_prompt_preserves_reviewed_generation_until_confirmation`: typing a reason cannot adopt a concurrent policy update.
- `reboot_preflight_refusal_requires_explicit_review_with_original_order_and_drain`: preflight is not permission to reboot.
- `run_results_keep_exact_attach_links_and_detached_accepted_runs_are_watched`: accepted work remains discoverable without selecting a substitute run.
- `stale_capture_and_call_results_cannot_replace_a_newer_review`: late callbacks cannot clobber current intent.
