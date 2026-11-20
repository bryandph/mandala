# Qualification evidence and outstanding acceptance gates

Recorded verification snapshot: `3ffd6fae11059886d9418d5c1d55e684a78c3f48` (later handoff work adds documentation and the opt-in blocking-work witness, not a production fix). Passing commands below are necessary evidence, **not implementation acceptance**. Tasks 3.6/4.4 retain the owned-blocking-work review gate below. The operator-approved fork amendment resolves 4.5 as a lock guarantee with an explicit inherited-socket limitation. The Mac sandbox worker failure is now baseline-proven pre-existing under coordinator decision 8; post-amendment package reruns are recorded separately. No rollout, PR or merge was performed.

## Acceptance gate and approved qualifications

1. **Operator-approved fork limitation (4.5 complete under signed option-A amendment):** process-lock ownership is not inherited. A deliberately paused pre-exec child may retain listener or accepted-socket copies, delaying takeover or followers' EOF/in-flight completion until exec/exit; socket-level failover latency is not bounded in that window. Acquisition stays fail closed, and exactly one authenticated replacement serves after release. Production launch paths must immediately exec, with only non-blocking fd-flag pre_exec hooks. The stronger serving-child witness remains ignored as documented limitation evidence, not reported as passing. No atfork or heartbeat-deadline redesign. See [fork-audit.md](fork-audit.md).
2. **Explicit macOS sandbox build (5.5):** Darwin loopback permission is fixed by the narrowly authorized `__darwinAllowLocalNetworking = true` attribute. The explicit sandbox build then passes context/MCP gates but fails the unchanged eval-worker git_reload test: C API initialization reports `Null pointer error`, then `eval worker unavailable: worker closed stdout` at commit A. Normal macOS builds and Linux sandbox builds pass. Pinned main `ecbec4da4b4cb8d5581e9da02dc2300c1a4a22ab`, with ONLY `__darwinAllowLocalNetworking = true` overridden, failed the same git_reload.rs:76 commit-A case with two Null-pointer init errors and worker-closed-stdout (0 passed/1 failed, 0.30s). This proves the failure pre-existing/out of scope under decision 8. Baseline log: `/tmp/lab218-baseline-mac-sandbox.log`, derivation `mvrxxcxlswdglqilksm6m7nvdiwpgrkc-mandala-0.1.0`; branch comparison: `/tmp/lab218-final-build-mac.log`, derivation `wkcmlsnp13qwbz2l4zzih99r7ivqkg42-mandala-0.1.0`. The approved Mac merge evidence is normal Mac build plus native full workspace; Linux sandbox remains CI authority. No eval-worker or further sandbox-policy changes were made. A cache hit after the normal build is not sandbox-rebuild evidence.

3. **Inner blocking work (3.6/4.4):** production MCP spawn_blocking jobs capture evaluator/observer state but no lease. An already-started blocking closure survives cancellation of its async dispatch parent. The opt-in `blocking_work::blocking_job_must_finish_before_a_replacement_factory_runs` witness fails natively with a replacement factory observing one old blocking job still active, despite the old wire call having terminated (0 passed, 1 failed, 0.01s; fixture thread cleanly released before assertion). It is preserved ignored by default pending broader-cancellation/lease-retention scope review, not hidden as passing. Existing teardown tests prove outer async-future/socket termination, not this inner-work guarantee. No process-wide evaluator cancellation or detached-supervisor changes were made.

## Preserved pre-fix evidence

Diagnostic patch SHA256 was verified as `25881535aa1ade310c45968e2250c4a0e2cccbc1ca2458f011da1b786d46ffea` before applying it unmodified in its own signed commit:

`130c29b31dd8b44a60151146918d183b1153ed5d` — `test(context): preserve pre-fix LAB-218 split-brain diagnostic`.

In a fresh Linux/WSL scratch clone, with an immutable copied executable, isolated HOME/state/runtime/temp and private loopback namespace, the original controlled witness failed: **0 passed, 1 failed**. Two same-flake authenticated endpoints actually executed calls: port 29800 returned `fast`; port 29802 returned `slow`. The supplied historical abrupt observation also remains valid: failure at trial 698 after 697 successful 32-contender trials. Later stochastic success is not disproof.

## Lifecycle decision

Signed investigation commit `087875a2a7522e8f37f1cf601d72914c5802d768` establishes that live-process host drop is reachable. `ContextSession::shutdown` replaces the shared role with Closed and consumes the host while clones/runtime remain alive; MCP/TUI library entry points shut down then return; discovery publication can fail after host start. Detached wire futures and cloned local dispatch snapshots previously escaped this lifetime.

The coordinator approved the conditional tracked-abort branch and narrowly included the leader-owned checkout watcher. The implementation tracks outer async call execution, rejects late local snapshots, stops/joins the watcher and dispatched async futures, cancels connection writers with their owner, waits for actual stream/future destruction, closes the listener and cleans discovery while still guarded, then releases those guards. The newly identified inner blocking-work gate above prevents claiming complete configuration-work ownership. Independent deploy supervisors are not cancelled. Detailed source evidence and decisions are in [lifecycle.md](lifecycle.md).

## Final Linux qualification

Environment: Linux `6.18.58.1-microsoft-standard-WSL2`, x86_64, 32 online CPUs. The repository was a fresh `/tmp/lab218-impl-3` clone of this branch, never an existing checkout. Tests ran from separate non-writable copied executables under:

```sh
unshare --user --map-root-user --net
ip link set lo up
# isolated HOME, MANDALA_FLEET_STATE, XDG_STATE_HOME, XDG_RUNTIME_DIR, TMPDIR
# every binary: --test-threads=32
```

Four owned `yes` processes supplied controlled CPU load (not claimed as full 32-core saturation). The final run recorded PIDs 2373543, 2373544, 2373545 and 2373546, and its EXIT trap stopped and reaped exactly those processes. Two repetitions each passed:

| Binary | Passed per repetition | Ignored qualification cases |
| --- | ---: | --- |
| mandala-context library | 22 | none |
| context | 7 | none |
| failover | 7 | none |
| election | 9 | none |
| lease_lifecycle | 7 | none |
| lease_order | 1 | none |
| process_exit | 4 | 1 documented socket-limitation witness |
| lifecycle | 7 | none |
| lab218_diagnostic | 1 | stress invoked separately |
| unavailable_stdio (MCP) | 2 | none |

Some counts include no-op subprocess entry points in their parent test process; the actual process/fork and IO/busy scenarios run in the named assertion tests.

The final opt-in abrupt stress passed **3000 trials with 32 contenders in 300.66 seconds**, under the same load/netns and `--test-threads=32`. Every trial checked exactly one authenticated serving endpoint and agreement between wire calls and all successful participants' dispatch results, not merely role flags. Earlier checkpoints independently passed two further 3000-trial runs (320.32s and 278.31s).

The final refusal preflight was also negatively qualified: outside the private namespace, the same immutable library executable failed explicitly in 0.50s on an unused `127.0.0.1:30928` timeout, before acquisition. Inside the namespace it passed immediately. It probes dedicated non-ephemeral ports without creating temporary listeners, requires three actual refused samples within twelve attempts, and never skips a timeout. Unit tests pin the finite collision-reselection budget and distinguish occupied samples from refusals.

## Native macOS and repository gates

All runs used isolated fixture state and injected coordination roots. The explicit native context run used 32 test threads and passed the same context counts above (22 library, 7 context, 9 election, 7 failover, 1 witness, 7 teardown, 1 listener-order, 7 lifecycle, 4 process-exit with the stronger fork witness ignored). Native frontend stdio tests passed 5; IO/busy error-only MCP tests passed 2 and checked all 19 advertised tools in both failure scenarios.

| Gate | Result |
| --- | --- |
| `cargo test --workspace` | PASS, canonical isolated HOME/state/runtime/temp and isolated NIX_REMOTE store |
| `cargo test -p mandala-context -- --test-threads=32` | PASS, counts above |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| `cargo fmt --all -- --check` | PASS |
| `nix fmt -- --check .` | PASS |
| `nix flake check` on macOS | PASS, two native checks; incompatible systems omitted normally |
| normal `nix build .#mandala-rs` on macOS | PASS, real check phase 7m52s; default sandbox=false |
| `nix build .#mandala-rs --option sandbox true` on Linux | PASS, real whole-workspace check phase 5m01s |
| explicit sandbox=true macOS build | FAIL at unchanged eval-worker initialization, baseline-proven pre-existing failure; not claimed as a pass |

Latest successful package outputs:

- macOS normal: `/nix/store/ssgsccglw0j3sq476032av83b0agw03i-mandala-0.1.0`.
- Linux sandboxed: `/nix/store/gzvyqkxp0j545jlqawh5hj2q16kdkyh2-mandala-0.1.0`.

The first explicit Darwin sandbox attempt denied `bind(127.0.0.1:0)` with EPERM before any root/home resolution. Previously, MCP initialization could appear successful only because a generic acquisition IO error degraded to effectful standalone service; discovery assertions revealed that no context existed. No full baseline sandbox pass is claimed. After the approved local-network attribute, those context gates passed and exposed the separate worker-init failure above.

The native isolated Nix store uses canonical `/private/tmp/...`, not symlink `/tmp/...`; the initial noncanonical fixture failed Nix's store-parent safety check and was corrected before the passing workspace run.

## Requirement-to-evidence mapping

| Delta requirement | Tests / evidence |
| --- | --- |
| One shared execution context per checkout | `promotion_race_has_exactly_one_winner`; deterministic `disappearing_foreign_occupant_invalidates_completed_scan` with real guard-attempt hook, gated owner and panic-on-loser factory; per-trial authenticated endpoint/dispatch assertions in `stress_abrupt_promotion`; process-exit factory count exactly one |
| Leader failover without losing running work | `idempotent_read_retries_once_after_promotion`; `mutation_surfaces_structured_failover_without_retry`; `call_issued_after_leader_death_fails_over_without_hanging`; `subscribe_stream_resumes_across_promotion`; actual `process_exit_promotes_once_and_retries_only_reads` (32 sessions, exactly 16 read retries, no mutation replay); frontend `native_deploy_survives_leader_death_and_attaches_after_promotion` |
| Context discovery and authorization | `stale_discovery_is_reclaimed_without_cleanup`; `derived_port_collision_walks_and_converges`; `missing_discovery_never_hides_a_walked_leader`; `discovery_token_mismatch_refreshes_instead_of_binding`; `busy_leader_is_never_usurped`; `token_rotation_cuts_old_connects_not_live_connections`; existing bad-token/hello/wire tests |
| Process locks exclude contenders without fork inheritance | `distinct_process_exclusion_and_sigkill_release`; `same_process_sessions_aliases_and_independent_flakes`; helper `fork_child_blocked_before_exec_cannot_retain_dead_parent_lock` (failed-exec too); hard-link/rename and injected post-open alias race with independent-process F_GETLK before/after owner release. The stronger paused-child serving witness is outside the operator-amended guarantee and remains ignored as limitation evidence; active characterization proves safe busy then exec/exit recovery, with accepted-socket EOF latency explicitly limited |
| Listener closure precedes leadership release | **Partial: inner blocking-work gate remains open.** Outer async/socket proofs: `dropped_host_retains_guard_until_its_unscheduled_listener_owner_exits`; `wire_drop_terminates_calls_and_background_before_new_factory`; `local_call_is_terminated_after_bounded_drain`; `retained_local_snapshot_cannot_invoke_config_after_shutdown`; `cancelled_shutdown_still_finishes_guarded_teardown`; `cancelled_acquisition_releases_its_guard_without_factory`; `factory_panic_and_publication_error_release_only_after_listener_close`; exhausted foreign ports; existing orderly-drain/discovery-cleanup tests |
| Coordination failures never create an unlocked leader | `different_discovery_roots_share_exclusion_and_fail_bounded`; `unsafe_roots_and_lock_files_fail_closed`; isolated missing-default-root/parent creation and unsafe-parent tests; filesystem denylist and native opened-fd admission tests; unsupported-errno and foreign-metadata validation; `io_and_busy_failures_initialize_then_refuse_every_tool_without_effects` (initialize OK, all 19 tools explicit errors, no run/audit dispatch; IO case proves listener/lease gone first) |
| Election safety requires coordinated upgrades | `docs/context-election.md`: stop all user/host frontends, upgrade every launch path, restart together; old binaries are not fenced; never unlink lock files as rollout/recovery. Separate operator rollout decision remains 6.3; no mixed-version safety claim |
| Context tests verify loopback refusal before election | shared `tests/common/refusal.rs`; unit classifier/budget/timeout test; explicit WSL default-namespace failure versus netns success; native/private-netns runs above. Production busy handling unchanged |
| Context election rendezvous has a stable private inode | account-home layout/override tests (HOME/client state/runtime variations, runtime absent); isolated secure default creation and missing injected-root failure; ownership/mode/type/symlink/FIFO/unsafe-parent checks; post-open root replacement and owned-inode alias race; `inode_is_persistent_and_lock_file_private`; inode assertions through replacement, shutdown and token rotation; `lock_contents_are_unchanged_and_never_ownership_metadata` |
| Coordination layout is distinct from serialized discovery compatibility | existing `discovery::tests::roundtrip_mode_0600_and_byte_format`, concurrent-writer and protocol stable-tag tests; unchanged discovery/protocol/identity source formats; existing workspace run/event metadata gates; lock contents confer no authority and inodes are not rewritten for rotation |

## Coverage limits and non-applicable API branches

Native foreign-owner inode creation would require privilege; tests inject ownership validation metadata instead and never chown live files. Unsupported-lock errno classification is injected, not a claim that an unsupported filesystem was qualified. Default creation uses an isolated account-home fixture; actual account-home resolution tests are read-only and never open/create the real coordination root.

The factory type is infallible (`FnOnce() -> HostConfig`); panic/unwind is tested instead of an unavailable Result-returning failure branch. `RunningHost::start` has no recoverable startup error after a bound listener; bind exhaustion, cancellation, panic and discovery publication cover reachable boundaries. Any future fallible API must preserve the same ordering.

## Scope, dependency and artifact review

- All commits are signed; the diagnostic remains a separate pre-fix history point before fixes.
- Cargo.lock adds only the existing `nix 0.31.3` edge for mandala-context; no package/version/source changes. The direct dependency enables no unnecessary features. `cargo tree -d` package-header sets match an archived baseline exactly: no new duplicates.
- The only flake edit is the explicitly approved Darwin local-network attribute and its comment. No flake.lock, CI, `.okf`, `.serena`, other-repository or canonical-spec changes.
- OpenSpec CLI was unavailable. Manual strict review verified exact modified requirement names, delta headings, 8 fleet-context requirements/32 scenarios after the operator-approved fork amendment and 2 state-format requirements/11 scenarios with WHEN/THEN coverage. Unaffected formats/retry/supervisor behavior remained covered by existing tests.
- Full commit list, final diff stat, clean status and scratch cleanup are reported to the coordinator at handoff. Acceptance remains blocked by the 3.6/4.4 blocking-work scope decision, not hidden as a deviation or rollout permission. Fork option A is an explicit operator-approved amendment, and the baseline-proven Mac sandbox worker failure is separately out of scope under the approved merge bar. All eight owned Linux scratch paths, including the clean `/tmp/lab218-impl-3` clone and copied-binary/root fixtures, were removed after checking no matching fixture process remained. The earlier intentionally unwritable-parent test directory needed owner-verified, no-symlink-following directory write-bit restoration solely for disposal; no live-home/state path was changed.
