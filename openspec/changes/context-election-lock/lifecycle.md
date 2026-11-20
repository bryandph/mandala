# LAB-219 lifecycle investigation (task 1.1)

Investigated baseline `ecbec4da4b4cb8d5581e9da02dc2300c1a4a22ab` before implementation.

## Conclusion and selected branch

A `RunningHost` **can be dropped while its process continues**. Select the live-process-drop branch: track dispatched calls with a JoinSet or abort handles, provide equivalent ownership for local calls, and prove actual termination and listener closure before releasing either leadership guard. An abort request alone is insufficient. This is bounded context teardown, not cancellation of independent detached supervisors or rollback of effects already performed.

## Ownership and teardown trace

- `host.rs:146-152`: `RunningHost` owns an accept JoinHandle, shared configuration, shutdown sender and optional discovery claim; the listener itself moves into the accept task (`host.rs:178-205`). `RunningHost::start` is public; a caller can drop its returned host without exiting.
- `lib.rs:179-196`: acquisition starts a host before publishing discovery. A discovery write error propagates through `?` and drops the host while the caller/runtime remain alive. Factory panic occurs before host start; cancellation/unwind and later caller drop must also be guarded by the implementation's ownership arrangement.
- `fleet.rs:153-189`: production ownership is `ContextSession -> Arc<SessionInner> -> Mutex<Role> -> Role::Leader { host, config }`. Last-session drop drops that role/host; task-held session clones can delay it, so frontend handle drop is not by itself a termination proof.
- `fleet.rs:228-235`: `ContextSession::shutdown(self, grace)` replaces the shared role with `Closed`, then consumes the host through `host.shutdown(grace).await`. Other session clones can remain alive. Shutdown completion drops the host and returns normally; cancellation of shutdown also drops its owned host in a live runtime.
- `fleet.rs:253-275`: reacquisition only replaces a follower, not a leader. There is no leader-demotion transition or autonomous leader restart path. Independent new sessions can acquire in the same live process after shutdown; the public API does not require process exit between sessions.
- `mandala-mcp/src/lib.rs:115-124`: production `run_stdio` awaits the stdio conversation, explicitly shuts the session down, then returns its result. It is a library async entry point, not a process-exit primitive.
- `mandala-tui/src/app.rs:227-230`, `explorer.rs:147-148`: production explorer returns from its event loop and invokes `shutdown_context` before terminal restoration and function return. `App` takes the context out; the session is explicitly shut down while background clones/process may remain alive. `adopt_context` (`app.rs:206-221`) also assigns its context field; implicit field/last-owner drops remain possible rather than being process-only ownership.

## Work which currently escapes teardown

- `host.rs:191` spawns connection tasks independently. `host.rs:458-459` independently spawns each wire call and awaits dispatch without selecting shutdown or retaining its JoinHandle. Socket/accept shutdown does not cancel these dispatch futures. `RunningHost::drop` (`host.rs:318-325`) signals shutdown and requests accept abortion only.
- `host.rs:292-315`: graceful shutdown bounds its drain, signals shutdown and waits for accept-task completion, but does not join/cancel wire dispatch. A call exceeding grace can keep running in the live process.
- `fleet.rs:171-175,239-244,285-288`: a local snapshot clones `config.dispatch` outside the role mutex and directly awaits it. Concurrent explicit session shutdown can close/drop the host while that local call retains and executes configuration. Wire unsettled accounting does not cover these local calls. Retained pre-shutdown snapshots must not start work after lease release.
- `mandala-mcp/src/context.rs:118-130,137-163`: the config factory starts a weak-handler checkout watcher; an iteration upgrades the handler and directly calls `call_tool_from(..., "reload", ...)`. A live upgraded iteration can retain the handler after host teardown. This leader-owned background configuration path also needs lease-aware teardown/dispatch routing; it is not an independent deploy supervisor. Reported at the task-1.1 gate for review of the narrowly required frontend seam.

Independent deploy supervisors are distinct run owners: `mandala-core/src/runner.rs:1027-1034,1081-1089,1249-1284` launch a dedicated supervisor process; `mandala-mcp/src/lib.rs:121-122` explicitly preserves launched runs across frontend exit. Do not cancel these supervisors as context-dispatch cleanup.

## Coordinator decision

Coordinator approved task 2 onward after reviewing this trace. The checkout watcher is narrowly included as leader-owned work: retain its JoinHandle in the lease, abort and await actual termination before guard release, without changing watch semantics or cancelling independent deploy supervisors. This implements the existing factory/config/local-dispatch lifetime requirement; it is not a deviation.

The coordinator also approved narrow inode-ownership bookkeeping under the same process-wide registry mutex as the keyed async-mutex map. Same-user tampering inside the mode-0700 root can otherwise redirect an open to another live lease's inode: merely rejecting and closing that descriptor silently releases its process record lock. Pre-open inode aliases now fail explicitly; a replacement found after open transfers the unexpected descriptor to the existing lease, which closes it only after releasing its original lock. No acquisition shares an existing lease. Hard-link, rename and injected open-race tests use an independent process's F_GETLK observation before and after teardown; deferred descriptors do not outlive their owner.

The coordinator chose runtime filesystem admission as the interpretation of unqualified locking: fstatfs the opened root descriptor; Darwin must report MNT_LOCAL, while Linux refuses known NFS, SMB/CIFS/SMB2, 9p, FUSE, Ceph, both AFS magics, Coda and NCP types. Other Linux filesystems are assumed local, not selected by an allowlist. Lock-time unsupported errno handling remains in place. Both injected and default roots use this policy; no network-home qualification is claimed.

For canonical path flakes, the coordinator also confirmed that every MCP acquisition error must avoid effectful standalone fallback while still answering protocol initialization. Acquisition now completes mandatory failed-host teardown first; MCP then logs the error loudly and serves its static tool list plus explicit structured acquisition errors from every tool call. It never creates an ordinary handler/evaluator for that fallback. Non-path references retain existing standalone behavior. The TUI retains read-only fallback except for coordination errors.

Inherited socket handling is on hold for an operator decision: see [fork-audit.md](fork-audit.md). No atfork code is implemented. The original stronger serving-child witness is preserved as ignored evidence; a separate active characterization proves bounded LeaderBusy with zero factories while the child is parked, followed by one authenticated replacement after exec or exit.

The coordinator lifted the flake exclusion for exactly one build-policy attribute, `__darwinAllowLocalNetworking = true` in mandala-rs. Native Darwin sandbox denied loopback bind with EPERM, which the previous effectful standalone fallback could mask during MCP initialization. This attribute permits isolated loopback tests within an otherwise enabled sandbox; no flake.lock or CI change is authorized. Both explicitly sandboxed and normal Mac builds are required, and Linux sandbox build remains the CI authority.

## Additional blocking-work scope gate

Production MCP effects start blocking-pool work in `effects.rs:234,255,279` (evaluator reload/evaluation and repository revision); `server.rs:741,1123` likewise starts blocking registry/snapshot work. Tokio cannot abort an already-started spawn_blocking closure merely by aborting and joining its async parent. Those closures capture their evaluator/work state, not the current election lease. The tracked async future and sockets can therefore stop while inner work remains active.

The isolated opt-in witness `blocking_work::blocking_job_must_finish_before_a_replacement_factory_runs` confirms this: the old wire call terminates, a replacement factory observes one old blocking job still live (required zero), then the fixture releases and finishes its blocking thread before failing the assertion. Native result: 0 passed, 1 failed, 0.01s. It is ignored by default as explicit pending-review evidence, not claimed as passing.

This is escalated under the spec's broader-cancellation-review rule. A narrow lease-retention/owned-blocking-work registration mechanism is the recommended next review direction: the guard must stay retained until owned work actually ends, with safe unavailability if it cannot stop. The exact bounded frontend/evaluator teardown policy needs review before implementation. The existing global shutdown_workers latch is terminal/process-wide and cannot simply be used for an in-process session restart or independent flake. Independent detached deploy supervisors remain excluded. Whether any already-launched work falls outside the guard contract requires an explicit decision, not an assumed carve-out. Tasks 3.6 and 4.4 are reopened as partial; no new cancellation/evaluator internals were changed.

## Required implementation evidence

Prove blocked wire calls and local calls are actually dropped/terminated before a contender obtains the released lease, including graceful-drain expiry, abrupt Drop, cancelled shutdown/acquisition and retained local snapshots. Keep listener and guards in a proven teardown ownership order. Test real process death separately with SIGKILL and fork-before-exec children; a Drop-based diagnostic is live-process connection loss, not process-death evidence.
