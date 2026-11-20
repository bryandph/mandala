# Qualification checkpoint (not implementation acceptance)

The implementation is not yet qualified. In particular, a serving endpoint's inherited TCP listener can outlive its parent in a child paused before exec; the process record lock itself is correctly released, but the unchanged busy-endpoint rule then prevents replacement. The stronger actual-serving-child test is preserved as ignored evidence at the coordinator's instruction, pending the operator decision. An active three-variant characterization proves safe LeaderBusy with no factory, followed by exactly one authenticated replacement after child exec or exit. Do not substitute the helper-only fork result for this gate or close inherited descriptors only in the test fixture to hide it.

## Preserved baseline

- Diagnostic patch SHA256 verified: `25881535aa1ade310c45968e2250c4a0e2cccbc1ca2458f011da1b786d46ffea`.
- Signed diagnostic-only commit: `130c29b31dd8b44a60151146918d183b1153ed5d`.
- Linux/WSL, fresh scratch clone, immutable executable, isolated HOME/state/runtime/temp and private loopback netns: original controlled witness failed (0 passed, 1 failed). Two same-flake authenticated endpoints executed calls: port 29800 returned `fast`; port 29802 returned `slow`.
- Supplied historical abrupt evidence remains valid: original failure at trial 698 after 697 successful 32-contender trials.

## Linux checkpoint at 22d5dda

Tests ran over SSH in the fresh scratch clone, not any existing checkout. Executables were copied to a separate scratch fixture and made non-writable before execution. `unshare --user --map-root-user --net` with `ip link set lo up` provided private loopback. Four owned `yes` processes provided controlled CPU contention; an EXIT trap terminated and waited for exactly those owned load processes.

All runs used `--test-threads=32`. Two repetitions each passed:

| Binary | Pass count per repetition |
| --- | ---: |
| mandala-context library | 19 |
| context | 7 |
| failover | 7 |
| election (lock/helper fork/security/layout) | 8 |
| lease_lifecycle | 6 |
| lease_order | 1 |
| process_exit (before the actual-serving-child addition) | 2 |
| lifecycle | 7 |
| deterministic serialization witness | 1 (stress ignored in regular run) |

The opt-in abrupt stress passed **3000 trials with 32 contenders**, in 320.32 seconds under the same load/netns. Every trial checked exactly one authenticated serving endpoint and agreement between wire calls and every successful participant's dispatch result, not only role flags.

The library includes independent-process F_GETLK observations proving that same-process hard-link/rename aliases, including an injected pre-open replacement race, do not accidentally release another live lease. Deferred descriptors are closed after their original owner releases, and do not outlive that owner.

These are checkpoint results, not final qualification after future socket-ownership or filesystem-policy changes.

## Native macOS checkpoints

- Lock/helper library and fork tests passed before integration (14 library + 6 integration).
- Latest targeted native run: 19 library, 8 election, 7 lease_lifecycle, 1 lease_order passed at 32 test threads.
- Existing context/failover/lifecycle suites passed with 7 tests each. The deterministic serialization witness passed.
- Actual process exit with 32 simultaneous sessions passed: one factory/serving endpoint, exactly 16 read retries, no mutation replay.
- Frontend stdio suite passed 5 tests natively.
- All-target workspace clippy with `-D warnings` passed at the latest checkpoint.
- `nix fmt -- --check .` passed (64 files).
- Full workspace testing exposed the unresolved actual-serving-child listener inheritance test; it is not a passing workspace gate.
- The first explicitly sandboxed macOS Nix build failed three stdio discovery assertions while handshakes succeeded. The diagnostic rerun's refusal preflight identified `bind(127.0.0.1:0)` returning EPERM under Darwin sandbox, before any root resolution: not a HOME/injection failure. The old generic standalone fallback masked the bind error for protocol handshakes, so those apparent successes did not prove a context existed; no full baseline sandbox pass is claimed. The coordinator authorized exactly `__darwinAllowLocalNetworking = true` on mandala-rs (no other flake/CI/input change), allowing honest sandboxed loopback tests. Sandboxed and normal Mac builds must now both be rerun, with the Linux sandbox build remaining CI authority.

## Explicit coverage limits and remaining gates

Native foreign-owner inode creation would require a privileged fixture. Current tests use synthetic ownership metadata validation, never chown live files. Unsupported-lock errno classification is injected; no claim is made that an actual unsupported filesystem was qualified. Default-root creation uses an isolated account-home fixture; production account-home resolution tests are read-only and do not open/create the real default root.

The host-config factory is infallible in its type (`FnOnce() -> HostConfig`); its panic/unwind is tested. `RunningHost::start` has no recoverable startup error after a successfully bound listener; binding exhaustion, cancellation, panic and discovery-publication errors are tested instead. A future fallible factory/start API must preserve the same teardown ordering.

Resolved coordinator decisions: opened-fd filesystem admission (Darwin MNT_LOCAL; Linux remote/FUSE denylist, others assumed local), and initialize-capable error-only MCP handling after every canonical-flake acquire error, with completed failed-host teardown first. Native library/classifier tests passed 21; error-only MCP stdio tests passed 2, including IO/publication and coordination busy, checking every advertised tool and no mutation/audit dispatch. Socket atfork remains on hold for the operator; the production spawn audit is in fork-audit.md. Pending qualification: final full workspace/format/clippy runs, Linux/macOS sandbox builds, native flake check, final requirement-to-test mapping and scratch-clone cleanup. No rollout, PR or merge is authorized by these results.
