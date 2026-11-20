# Production child-spawn audit and inherited-listener characterization

The operator approved option A: the guarantee covers non-inherited process-lock ownership, while inherited listener/accepted-connection copies may delay socket takeover or EOF until child exec/exit. This is recorded in the signed amendment to the change's fleet-context requirement, design §6 and task 4.5. No atfork or heartbeat-deadline redesign is authorized or present.

## Result

Every production child creation found in the Rust workspace goes through `std::process::Command` or `tokio::process::Command`. No production raw `fork`, `nix::unistd::fork`, daemonize, `forkpty` or `setsid` call exists. The raw forks in context qualification are isolated test fixtures only. Clap's `Command` builders and thread/task spawns are not process spawns.

The two production `pre_exec` hooks are in `mandala-core/src/runner.rs`: `start_supervised` and `start_inline`. Both call only `inherit_launch_lock(fd)`, which makes two `fcntl` calls (F_GETFD/F_SETFD) to clear CLOEXEC on the independent deploy launch lock. They contain no wait, sleep, input read, allocation, application callback, gate, or deliberate pause. They are followed immediately by the standard library's exec path. A scheduler stop, resource pressure or a slow kernel operation can still extend any pre-exec interval; there is no hard latency proof.

Detached deploy supervisors are executable children, not fork-only descendants. The frontend forks/execs the supervisor with the short launch-lock hook; the execed supervisor then spawns the engine through the process-group helper. Process-group assignment uses `process_group(0)`/setpgid, not setsid or a double-fork daemonization. Independent deploy supervisors remain untouched by context teardown.

## Toolchain routing (verified from installed source)

The devshell's Rust standard-library source `std/src/sys/process/unix/unix.rs` was inspected, not inferred from command names. `Command::spawn` first tries `posix_spawn` and falls back to fork+exec. Its eligibility check rejects pre-exec closures, uid/gid/groups/chroot changes, and an overridden PATH with a bare executable. Relative executable + cwd on macOS, missing addchdir support and Linux libc/pidfd capabilities can also select fork+exec. Process-group assignment alone is supported in the posix_spawn path.

Tokio 1.53.1 `src/process/mod.rs:865` delegates to `self.std.spawn()` before adding asynchronous reaping/stdio. Thus an ordinary site below is **posix_spawn eligible, otherwise fork+immediate exec**, not guaranteed always posix_spawn. The forced-hook sites are **fork+immediate exec** on both platforms. On a failed exec, Rust writes a small errno message to its error pipe and `_exit(1)`s; it does not run a long-lived child continuation.

| Production constructor/path | Purpose / spawning boundary | Route and child-side work |
| --- | --- | --- |
| `mandala-core/src/eval.rs:370` | lazy external evaluator worker | std spawn, eligible; stdio setup then exec |
| `mandala-core/src/eval.rs:519` | cold `nix eval` subprocess fallback | std output, eligible; exec |
| `mandala-core/src/drift.rs:431` | git checkout/revision reads | std output, eligible; exec |
| `mandala-core/src/drift.rs:727` | snapshot survey ansible-playbook | std status with cwd/env, eligible subject to std conditions; exec |
| `mandala-core/src/runner.rs:1056` | execed supervisor starts deploy engine | `spawn_engine_handoff`, process_group(0), eligible; exec |
| `mandala-core/src/runner.rs:1275-1285` | frontend starts detached deploy supervisor | **forced fork+exec** by `pre_exec(inherit_launch_lock)`; only two fcntls before exec |
| `mandala-core/src/runner.rs:1319-1331` | inline deploy engine path | **forced fork+exec** by the same hook; only two fcntls before exec |
| `mandala-core/src/runner.rs:1626-1645` | registered command runs (ansible/actions) | Tokio spawn with stdio/cwd, eligible; exec |
| `mandala-core/src/campaign/source.rs:30` | git/nix source operations | `output_std`, process_group(0), eligible; exec |
| `mandala-core/src/campaign/api.rs:311-333` | campaign verification survey | `spawn_std`, process_group(0), eligible; exec |
| `mandala-core/src/campaign/execution.rs:364-372` | approved reboot/action command | `spawn_std`, process_group(0), eligible; exec |
| `mandala-core/src/engines/cache_publish.rs:249-251` | controller copy/publication | `discard_output_std`, process_group(0), eligible; exec |
| `mandala-core/src/engines/deploy.rs:874,935` | profile resolution and local build projection | `output_std`, process_group(0), eligible; exec |
| `mandala-core/src/engines/deploy.rs:980` | derivation log capture | `spawn_std`, process_group(0), eligible; exec |
| `mandala-core/src/engines/deploy.rs:1102-1108` | streamed native build subprocess | `spawn_std`, process_group(0), eligible; exec |
| `mandala-core/src/engines/deploy.rs:2073-2093` | batch build CLI | std status, eligible; exec |
| `mandala-core/src/engines/deploy/vm.rs:38` | owner artifact closure validation | Tokio `process::output`, process_group(0), eligible; exec |
| `mandala-core/src/vm/owner/native.rs:22` | store validation/rooting | Tokio `process::output`, process_group(0), eligible; exec |
| `mandala-core/src/vm/owner/native.rs:47-53` | immutable owner adapter | Tokio `process::spawn`, process_group(0), eligible; exec |
| `mandala-core/src/vm/owner/client.rs:36-40` | remote owner protocol session | SSH Command built by deploy helper, Tokio `process::spawn`; eligible; exec |
| `mandala-deploy/src/deploy.rs:215` and `owner_session_command` | SSH activation/check/owner commands | shared Tokio process helper, process_group(0), eligible; exec; arbitrary command override env can make PATH fallback select fork |
| `mandala-deploy/src/push.rs:197` | nix copy subprocess | shared Tokio process helper, process_group(0), eligible subject to override env; exec |
| `mandala-mcp/src/effects.rs:288` | captured snapshot survey | Tokio output with cwd/env, eligible; exec |
| `mandala-mcp/src/effects.rs:305` | ad-hoc fleet action | Tokio output with cwd/env, eligible; exec |
| `mandala-tui/src/app.rs:1381-1393` | task pane command | Tokio spawn with cwd/env, eligible; exec |
| `mandala-tui/src/explorer.rs:318-330` | survey pane command | Tokio spawn with cwd/env, eligible; exec |
| `mandala-tui/src/nom_pane.rs:157-165` | nom renderer on raw openpty slave stdio | std spawn, eligible; no pre_exec/setsid/forkpty; exec |

Shared process-group implementations: `mandala-deploy/src/process.rs:87-116` (std/engine handoff) and `193-221` (Tokio). They serialize parent registration and configure setpgid; they do not add child-side application hooks. Threads that drain logs or forward signals are not fork-without-exec paths.

This covers workspace-owned production Rust spawn sites, not arbitrary internals of external executables after exec or third-party native libraries. The evaluator's Nix C API runs in an already-execed evaluator worker, not in the context leader; inherited CLOEXEC context descriptors are closed before that worker begins.

## Native behavioral characterization

`inherited_listener_fails_closed_until_child_exec_or_exit` passed natively on macOS in 51.12 seconds. Three explicit barriers were exercised: paused-before-exec then successful exec; failed exec followed by a deliberate pause then successful exec; paused-before-exec followed by child exit.

For each case:

1. A real helper process serves a context endpoint; a follower successfully pings it.
2. The helper has forked a child and acknowledged that the child is parked before exec (or after failed exec). The parent is terminated while the child remains alive.
3. The test process can obtain the process record lock, proving that the child did not retain it.
4. Context replacement instead returns `LeaderBusy` within the existing busy window, with no factory invocation and no new discovery publication. The inherited listener has not become another authenticated serving leader.
5. The controller releases the child to exec or exit. The old listener actually closes; replacement then succeeds and exactly one authenticated endpoint returns the replacement dispatch result. In exec variants the child is still alive while replacement serves, demonstrating CLOEXEC recovery rather than child-death recovery.

Accepted connection descriptors can also be inherited. `client.rs:199` waits for frames/EOF and `:307` waits for call settlement; heartbeat receipt is counted, not used as a missing-heartbeat deadline. A deliberately held child that already inherited an accepted stream can therefore delay followers' EOF and in-flight completion until exec/exit as well. The active characterization forks before its follower connects, so it does not qualify that accepted-stream bounded-failover case. This is included in the operator-approved documented limitation, not an implemented atfork or client-timeout redesign.

The stronger original test `serving_parent_exit_allows_replacement_while_preexec_child_remains_alive` remains ignored with an explicit documented-limitation explanation under option A. Running it before socket-policy resolution exposes `LeaderBusy`, not an overlapping endpoint. It is not claimed as passing. This is distinct from the traditional-record-lock helper tests, which prove the lock is not inherited even with a live pre-exec child.
