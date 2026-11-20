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

## Required implementation evidence

Prove blocked wire calls and local calls are actually dropped/terminated before a contender obtains the released lease, including graceful-drain expiry, abrupt Drop, cancelled shutdown/acquisition and retained local snapshots. Keep listener and guards in a proven teardown ownership order. Test real process death separately with SIGKILL and fork-before-exec children; a Drop-based diagnostic is live-process connection loss, not process-death evidence.
