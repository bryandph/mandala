# Leader-owned blocking-work audit (scope decision pending)

No cancellation or evaluator implementation changes are made by this audit. It covers all 13 `spawn_blocking` sites found in production Rust: seven can belong to the shared leader/configuration path, while six are client-UI/standalone work outside that ownership boundary. A further read-only derivation-resolver thread is noted below.

## Conclusions for the operator

A blanket side-effect-free carve-out is **not clean**:

- `effects/campaign.rs:185` was an additional leader-owned site beyond the original five. Its closure can publish campaign manifests, persistent GC roots, verification run metadata/reports/logs and survey snapshots, and can start Nix/Git/ansible processes. These changes can occur after the async parent is cancelled and are visible to a replacement leader without consuming the returned value.
- Evaluator closures can start, reload/terminate and reap per-evaluator subprocesses. Their native backend locks flakes virtually, but the selectable subprocess backend uses `nix eval` without `--no-write-lock-file`: a controlled isolated fixture confirmed that those flags create `flake.lock`. Thus these sites cannot be certified process-/source-state-free across supported backends.
- `repo_rev` starts Git and can refresh the Git index through `git status --porcelain`; a controlled no-commit fixture confirmed index-byte changes from an ordinary status refresh.
- The registry-list/snapshot closures are genuinely read-only with respect to durable run/campaign/discovery state. Their private results are discarded after outer cancellation; they are the plausible narrow carve-out candidates, subject to the operator's definition.
- All current sites merely await `spawn_blocking` JoinHandles. Dropping the async parent neither stops a started closure nor explicitly aborts a queued closure. A carve-out limited to **already-started** work cannot silently include queued jobs that begin after teardown.

Option A (retain the lease until registered owned blocking jobs finish) therefore needs to cover at least mutation/process-bearing jobs and the leader watcher's probe/reload chain. It can delay takeover by a full evaluation or survey; there is no general finite worst-case bound. Option B cannot cover the campaign bridge or all evaluator/Git work as side-effect-free. This is a decision input, not an implemented policy. Independent detached deploy supervisors must remain independent; global `shutdown_workers` is terminal/process-wide and is not a session-restart mechanism.

## Common result-publication boundary

`MandalaHandler::call_tool_from` (`server.rs:236-276`) emits start before awaiting dispatch, then emits settle/audit and returns a result only after dispatch finishes. Inventory installation (`server.rs:404-420`) and expected-cache saving (`server.rs:606-617`) are async-parent continuations, not inside the evaluator closures. If the parent future has actually been aborted/joined before lease release, those continuations do not resume: an orphaned closure's returned value is discarded. A continuation that ran before cancellation still runs under the outer task's lease until termination.

That does **not** undo direct work done inside a closure. In particular, campaign state publication does not depend on the returned value being consumed. The current opt-in `blocking_work` witness demonstrates actual inner-work overlap, even while its outer wire call has terminated; it does not claim durable writes were needed to trigger that overlap.

## 1. `mandala-mcp/src/effects.rs:234` — fresh inventory

- **(a) Work:** clone the config's `Arc<Mutex<Evaluator>>`, lock it, `reload()` the warm evaluator, then evaluate/validate `<flake>#mandala` through `Inventory::from_evaluator` (`inventory.rs:373`).
- **(b) Durable/shared writes:** no direct fleet cache, discovery, campaign or run-registry write in this closure. Native worker `nix.rs:168-205` uses `LockMode::Virtual`, explicitly not writing `flake.lock`; ordinary Nix fetch/store/derivation effects and fetcher caches remain possible. The selectable subprocess backend (`eval.rs:536-543`) lacks `--no-write-lock-file` and can create/update the source lock file: the isolated fixture described below confirmed creation. It is therefore not unconditionally source-state-free. Warm EvalState changes are private to this old evaluator, not the new leader's evaluator.
- **(c) Processes/signals:** `reload` drops/kills/reaps the old worker; the next request lazily spawns a new `mandala-eval-worker`. Workers are per Evaluator (the Arc is shared within one RealEffects), not one globally shared worker. Worker Drop deregisters the still-unreaped PID, kills and waits (`eval.rs:482-495`); timeout and transport recovery also drop/reap, and transport recovery retries once. Subprocess mode starts/waits Nix commands instead. Normal Nix evaluation may request store realisation/IFD through its store; this is not a fleet-deployment launch by the closure.
- **(d) Duration:** cached/small evaluation can be short; cold fleet evaluation can take seconds to minutes. Worker response timeout defaults to 600s per roundtrip (`eval.rs:99-117`), and `MANDALA_EVAL_TIMEOUT=0` disables it. Mutex queueing, process startup, pipe writes, reaping and filesystem access are outside a whole-job deadline. Subprocess mode has no command timeout. General worst case: unbounded.
- **(e) Publication after teardown:** the Inventory result is installed only in the awaiting async handler; after that future is stopped, no inventory swap, expected-cache write, settle event or result occurs from this result. The closure can still finish evaluator/process/store/source-lock effects in (b)/(c), and the captured evaluator Arc can outlive the handler until it finishes.

## 2. `mandala-mcp/src/effects.rs:255` — expected toplevels

- **(a):** lock the same per-config evaluator and run `drift::eval_expected` (`drift.rs:350-364`), validating member names and evaluating their out paths. There is no unconditional reload at this site.
- **(b):** the closure does not call `save_expected` or publish `.expected.json`; it returns `Toplevels`. Native virtual-lock/store effects and subprocess source-lock effects have the same caveats as site 1. Subprocess batch failure can cause separate per-member eval invocations (`eval.rs:570-635`).
- **(c):** lazily spawn/recover the evaluator worker or start Nix subprocesses. Timeout/reload/final evaluator Drop kill/reap its own worker as above; no independent deploy supervisor is signalled by this closure.
- **(d):** full fleet eval can take minutes; default worker roundtrip is 600s, configurable/unbounded as above. Mutex wait and subprocess/per-member fallback make the complete job's worst case unbounded, not a 600s total guarantee.
- **(e):** complete expected results are saved by the async continuation in `server.rs:606-617`; that save cannot run after its containing dispatch future has stopped. The orphaned result is discarded, but process/store/source-lock work can still finish independently.

## 3. `mandala-mcp/src/effects.rs:279` — repository revision

- **(a):** run `drift::repo_rev`: `git rev-parse HEAD`, then `git status --porcelain`, returning a revision optionally suffixed `-dirty` (`drift.rs:369-379,431-445`).
- **(b):** no fleet/registry/discovery/campaign write is requested. However Git status may refresh its index/stat cache; optional locks are not disabled here. A controlled temporary repository with no commit, system/global config disabled and fsmonitor disabled showed `.git/index` checksum change after touching a staged file and running status. No refs or tracked file content changed, but it is still shared Git state a later leader can read. Arbitrary user Git configuration is not made side-effect-free by this wrapper.
- **(c):** starts Git subprocesses through `Command::output`; normal completion waits/reaps them. No cancellation signal is sent to that child when the async parent disappears. Configured Git helpers/fsmonitor are not suppressed by this wrapper.
- **(d):** normally short for a cached small checkout, but status of a large worktree can take seconds or longer. No subprocess/filesystem timeout; worst case unbounded.
- **(e):** the returned revision cannot be installed/emitted by a stopped async parent; Git's index/cache activity is direct and can nevertheless finish after teardown.

## 4. `mandala-mcp/src/server.rs:741` — run listing

- **(a):** `registry::list_runs`, create read-only `ObservedRun`s, call `run_snapshot(...Listing)`, return a Value list. `list_runs_in` enumerates/reads/sorts directories; it does not prune (`registry.rs:243-277`).
- **(b):** no durable writes. `ObservedRun::poll/liveness` (`registry.rs:365-433`) rereads metadata, updates only private in-memory info/tailers and judges liveness; it never calls `write_meta/update_meta`. `EventTailer` (`runner.rs:571-720`) reads event/VM/duration files and updates its private forest. MCP-created observers have `nixlog_sink=None`, so no presentation callback publishes output.
- **(c):** no external subprocess start/termination. Run liveness uses signal-0 existence/permission probing, not a delivered signal (`registry.rs:124-138`). Parsing Nix log records can start the read-only `mandala-drv-resolver` std thread (`runner.rs:507-535`), which only reads/parses immutable `.drv` files and sends private channel results.
- **(d):** usually small metadata/log reads, but first-time full logs, many run directories, large forests or blocked filesystem IO have no whole-job bound. The output's listing limit does not bound directory enumeration or bytes replayed.
- **(e):** returns only private Value/observer snapshots. If parent is stopped, the result is dropped: no shared registry update, event or new-handler state installation. Dropping an observer closes resolver channels; an already-running derivation read can finish and then stops on failed result send. No new leader consumes that private result.

## 5. `mandala-mcp/src/server.rs:1123` — one blocking snapshot

- **(a):** move one `ObservedRun` into `run_snapshot`, returning the updated private observer plus Value, so a live caller can preserve tail offsets across polls.
- **(b):** same read-only paths as site 4, including command logs, events, VM evidence and duration-cache loading. No run/campaign/discovery/cache write is performed by `run_snapshot`, `command_snapshot`, `ObservedRun::liveness` or `EventTailer::poll`.
- **(c):** same signal-0 liveness and possible read-only resolver thread; no external process launch or real signal.
- **(d):** an incremental poll is normally short, but initial/large log replay or filesystem stalls are unbounded. The surrounding `deploy_status` wait loop's deadline does not preempt an individual blocking job.
- **(e):** after async-parent termination, the returned observer/Value is discarded rather than published; resolver channels/private state drop as above. No late inventory swap, audit settle or shared event is produced from that orphaned result.

## 6. `mandala-mcp/src/effects/campaign.rs:185` — additional effectful campaign bridge

- **(a):** call synchronous `call(flake, operation, args)` for campaign_create/inspect/retry/reboot/exclude/close/verify/drift. The later async continuation (`should_start` and DeployRun supervisor launch) is outside this closure.
- **(b):** **not clean**. Create freezes source/captures and binds a campaign; `Capture::bind` (`source.rs:1318-1333`) publishes campaign state and persistent roots via `Store::create_with_actor`. Source freezing writes capture files and registers GC roots (`source.rs:88-125,479-525,803-845`), beyond merely computing a store path. Exclude/close use durable `Store::transaction` (`api.rs:205-263`, `campaign.rs:1100-1159`); reconcile-only retry can settle persisted attempts (`execution.rs:591+`). Verify (`api.rs:378-525`) writes prepared/linked campaign state, run meta, verification.json, output logs, terminal meta and manifest verification/generation data. Refresh/drift surveys can update shared fleet snapshots. Per-campaign locks/generation checks still protect those transactions, but are not the context lease and do not make the closure side-effect-free.
- **(c):** may start Nix, nix-store, nix-instantiate and Git for capture/GC rooting/source checks. Verify/refresh can start ansible-playbook via the process-group helper (`api.rs:311-349`), update its run PID, wait and reap. These are real external processes, not just computation. Actual deploy/reboot supervisor launch after the awaited result will not happen if that async continuation was already cancelled; already-launched independent supervisors are a separate owner and remain untouched.
- **(d):** simple manifest policy changes may be short; source capture/evaluation and host surveys can take minutes. The native command wrappers wait without a deadline; filesystem/process waits make worst case unbounded. Campaign try-lock contention itself fails promptly as WouldBlock, but this is not a bound on the rest.
- **(e):** **direct publication survives cancellation**: campaign manifests, roots, verification/run reports and survey snapshots are written inside the closure, so a new leader can observe them after the old async parent/lease has gone. Discarding the returned Value suppresses subsequent MCP settle/audit/result and supervisor-launch continuation, not those durable writes. A blanket option-B carve-out cannot cover this site.

## 7. `mandala-core/src/drift.rs:413` — leader checkout-watch probe

- **(a):** `spawn_repo_head_watch` runs `repo_head` on the pool once per one-second cadence. This is leader-owned when launched by the HostConfig factory through `spawn_handler_head_watch`, and client-owned when used by the TUI's separate context-free watcher.
- **(b):** the probe calls only `git rev-parse HEAD`, not status: no source/index/registry/campaign/discovery write requested. It does not reload inventory inside the pool closure.
- **(c):** starts/waits a Git process, with no host-drop-aware cancellation signal. It is not external-process-free.
- **(d):** normally cheap; no Git/filesystem timeout, so worst case unbounded. The one-second cadence is not a probe deadline.
- **(e):** after the watcher's async future is aborted/joined, its late head result is discarded and `on_change` cannot newly begin a reload. If a reload callback already started, its `fresh_inventory` work has site 1's issues. A scope mechanism must cover both the probe and callback chain; wrapping only the five original MCP sites is incomplete.

## Other six pool sites (not shared-leader/config ownership)

These must not be conflated with the above merely because the same process can currently be a leader. They are actor/UI/standalone tasks which can exist for observers or without a ContextSession:

| Site | Work; durable/process behavior; duration; result publication |
| --- | --- |
| `mandala-tui/src/context.rs:237` | Client-side post-RPC load metadata: repo_rev plus load_expected (read cache). Git can refresh index and is waited; no fleet cache write here. No hard deadline. An independent UI load task can still emit its private LoadFinished event if the App is alive; it is not the shared HostConfig broadcast/cache. Dead App channel send fails. |
| `mandala-tui/src/explorer.rs:209` | Context-free/fallback local inventory evaluation and cache inspection. Can start/reap a local evaluator and perform normal Nix/source-lock effects as above; no save_expected here. Minutes/unbounded worst case. Sends only its UI LoadFinished event; it is not cancelled by a shared host's Drop. |
| `mandala-tui/src/explorer.rs:229` | Context-free expected eval. In addition to evaluator/Git processes, it can directly save `.expected.json` after a complete eval, which a later frontend may read. Minutes/unbounded. It emits UI DriftEvalFinished; this is independent fallback work, not a leader-dispatch job. |
| `mandala-tui/src/deploy.rs:191` | Dedicated deploy-entry selector resolution before the App/context is created. Local evaluator/store/source-lock caveats; minutes/unbounded. Returns selector to the awaiting standalone deploy frontend; cancelled parent does not proceed to create/start the DeployRun. Not shared context ownership. |
| `mandala-tui/src/app/campaign.rs:72` | Explicit UI frozen-source capture. Can write temporary capture trees/GC roots and start Nix/Git, with minutes/unbounded latency. Result can be offered to a live App as CampaignCaptured; dropped result releases TempRoots. No campaign bind/host deployment in this closure. Actor-owned review, including observer mode. |
| `mandala-tui/src/app/campaign.rs:108` | Explicit reviewed capture.bind: writes a campaign manifest and persistent roots through Store even for an observer/standalone UI, then the async continuation may send a context campaign_retry or show preview. No deploy launch inside bind, but Nix GC-root commands run. No whole-job bound. Manifest remains visible independently of UI result; a live App can receive CampaignResult. This is existing actor-owned binding protected by campaign store policy, not a shared HostConfig job to blindly cancel on leader teardown. |

## Evidence and limits

- Native opt-in blocking-work witness: 0 passed/1 failed in 0.01s, replacement factory observed one live old blocking job after outer wire termination; the test releases/finishes its owned thread before asserting. Default remains ignored and explicitly marked pending scope review.
- Controlled Git fixture: no commit, system/global config and fsmonitor disabled; ordinary `git status --porcelain` changed `.git/index` checksum after a timestamp-only touch. Fixture removed.
- Controlled Nix fixture, isolated HOME and canonical private NIX_REMOTE store: exact subprocess aggregate flags (`nix eval --no-warn-dirty --json <fixture>#mandala`) created `flake.lock`; no network input or fleet effect, fixture removed. Native worker Virtual lock mode was verified in source, not inferred from CLI behavior.
- Typical duration labels above are qualitative, not benchmarks. The 600s worker setting is a response timeout, not a universal cancellation or takeover bound. No code, process or store behavior is assumed rollback-safe merely because its caller's Future was dropped.
