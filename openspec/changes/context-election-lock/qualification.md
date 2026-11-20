# Final implementation qualification and operator handoff

Full qualification snapshot: `9d12e6584874d623080381fafc294f4f4f03ef58`. A final review follow-up adds observability only: identity/count/kind when retirement waits, and elapsed wait after both guards are actually released. Its native mandala-context suites (70 passes), workspace clippy `-D warnings` and fmt were rechecked; targeted stderr validation confirms both diagnostics (`/tmp/lab218-retirement-observability*.log`). The coordinator performs the final Mac build/PR CI after that small follow-up; package results below identify the full qualification snapshot. Tasks through 6.2 are complete under the explicit operator amendments and baseline exception below; 6.3 remains the operator's separate rollout decision. No PR, merge, rollout, live coordination/state mutation or restart was performed.

## Approved decisions, not hidden deviations

- **Fork option A:** signed amendment `ce9061b541dde72af3270d9b7418f384fb117f42` scopes non-inheritance to the leadership LOCK. A child paused before exec can retain listener/accepted-socket copies, delaying takeover or followers' EOF/in-flight completion until exec/exit without a socket-level latency bound. Acquisition stays fail closed. Production launch paths immediately exec through Command/tokio Command with only non-blocking fd-flag application hooks. No atfork/heartbeat redesign. The stronger parked-child serving witness stays ignored as documented limitation evidence. Full source/toolchain audit: [fork-audit.md](fork-audit.md).
- **Blocking option B:** signed amendment `9e0d8c9540bf56cdc3e8f9c465e6f11c6b440adf` requires registered process/state-bearing blocking work, queued cancellation and actual completion/capture destruction before unlock. Only the two audited registry snapshot computations have the precise read-only/result-discard carve-out. Implemented by `f0dc018b692e4e1e6f228c3dddc7e753ab7f94bd` and `9d12e65`; the former opt-in failing witness is now five always-run passing guards. No evaluator internals, new global shutdown_workers call or independent-supervisor cancellation. Full audit/rationale: [blocking-work-audit.md](blocking-work-audit.md).
- **Darwin build-policy exception:** the coordinator authorized only `__darwinAllowLocalNetworking = true` plus its comment in mandala-rs. No CI/flake.lock changes. It exposes honest context tests instead of allowing a bind EPERM to be masked by effectful standalone fallback.
- **Mac sandbox baseline exception, decision 8:** the unchanged eval-worker git_reload failure was reproduced on pinned main with only that loopback attribute. It is pre-existing/out of scope and not claimed as a passing sandbox build. The approved Mac merge evidence is normal Mac build plus native full workspace; Linux sandbox remains CI authority. Details/logs below.
- **Existing dependency edge:** Cargo.lock adds only the already-locked `nix 0.31.3` edge for mandala-context, with no package/version/source change and no unnecessary direct features. Baseline/current `cargo tree -d` package headers match: no new duplicates.

## Preserved pre-fix evidence (first commit for reproduction)

`130c29b31dd8b44a60151146918d183b1153ed5d` — signed diagnostic-only commit, before any fix. Supplied patch was applied unmodified after SHA256 verification:

`25881535aa1ade310c45968e2250c4a0e2cccbc1ca2458f011da1b786d46ffea`.

On Linux/WSL in a fresh scratch clone, using an immutable copied executable, isolated HOME/state/runtime/temp and private loopback namespace, the original controlled witness failed **0 passed / 1 failed**. Two same-flake authenticated endpoints actually executed calls: port 29800 returned `fast`; port 29802 returned `slow`. Supplied historical abrupt evidence remains valid: failure at trial 698 after 697 successful 32-contender trials. Later stochastic success is not disproof.

## LAB-219 lifecycle conclusion

Signed investigation `087875a2a7522e8f37f1cf601d72914c5802d768` proves a host can be dropped while its process continues. `ContextSession::shutdown` replaces the role with Closed and consumes the host while clones/runtime remain alive; MCP/TUI library entry points shut down then return; publication can fail after host start. Detached wire futures and retained local dispatch snapshots previously escaped host ownership.

The selected branch tracks async execution and watcher ownership, rejects late local snapshots and orders actual listener/connection closure, async-call termination, watcher stop, registered-job completion and lock release. Registered blocking functions/captures keep the descriptor/keyed mutex independently of their cancelled parent/runtime. Queued functions cannot start after admission closes. Resource shutdown can return while a retirement owner still holds exclusion for running work, preserving bounded frontend quit; failed-publication cleanup waits for full retirement before the MCP error server. Exact source trace/decisions: [lifecycle.md](lifecycle.md).

The historical blocking witness at `a6598fab9caed7c7eb4a3c0674dbd42bff2109aa` failed on both OSes (0 passed/1 failed, 0.01s), observing one old blocking job at replacement-factory entry. It is preserved in history. The current always-run suite proves that case corrected, plus queued registered/snapshot cancellation, read-only takeover/result discard, panic cleanup and runtime-cancellation retention.

## Final Linux evidence at 9d12e65

Environment: Linux `6.18.58.1-microsoft-standard-WSL2`, x86_64, 32 online CPUs. Fresh `/tmp/lab218-impl-5` clone; no existing checkout was used. Binaries were prebuilt, copied to a separate fixture and made non-writable. Every test acquire/reacquire uses isolated pre-created roots; the default-creation test uses an isolated account-home fixture.

```sh
unshare --user --map-root-user --net
ip link set lo up
# isolated HOME, MANDALA_FLEET_STATE, XDG_STATE_HOME, XDG_RUNTIME_DIR, TMPDIR
# every binary: --test-threads=32
```

Four owned `yes` processes supplied controlled CPU load (not claimed as full 32-core saturation): PIDs 2449110, 2449111, 2449112, 2449113. The EXIT trap stopped and reaped exactly those processes. Two repetitions each passed:

| Binary | Passed per repetition | Ignored qualification cases |
| --- | ---: | --- |
| mandala-context library | 22 | none |
| blocking_work | 5 | none |
| context | 7 | none |
| failover | 7 | none |
| election | 9 | none |
| lease_lifecycle | 7 | none |
| lease_order | 1 | none |
| process_exit | 4 | 1 documented inherited-socket limitation witness |
| lifecycle | 7 | none |
| lab218_diagnostic | 1 | stress invoked separately |
| unavailable_stdio (MCP) | 2 | none |

Some counts include no-op subprocess entry points in their parent test process; actual process/fork and IO/busy scenarios run in the named assertion tests.

Final **3000 trials with 32 contenders passed in 216.48s**, under the same load/netns and 32 test threads. Every trial checks exactly one authenticated serving endpoint and common wire/local dispatch results, not only role flags. Earlier independent checkpoints also passed 3000 trials in 320.32s, 278.31s and 300.66s.

Refusal preflight negative qualification: the same immutable library executable outside private netns failed explicitly in 0.50s at unused `127.0.0.1:30928`, before acquisition; inside netns it passed immediately. Current preflight never creates temporary listeners, probes dedicated non-ephemeral test ports, requires three actual refusals within twelve attempts, discards occupied samples without counting them, and immediately rejects every timeout/other error. Unit tests pin the finite budget and strict classification. Production busy-leader handling is unchanged.

Raw final log: `/tmp/lab218-policy9-linux-qualification.log` on the coordinator Mac; package build log `/tmp/lab218-policy9-build-linux.log` on WSL.

## Native macOS and repository gates

The explicit native context run used isolated canonical HOME/state/runtime/temp and 32 test threads: **70 passed** across the same context binaries above (including all five blocking guards), plus the documented fork-limitation witness ignored and stress run separately on Linux. Native frontend stdio suite passed 5; TUI context_flow passed 14; IO/busy error-only MCP suite passed 2, checking all 19 advertised tools in both failure scenarios. Proxy/failover/parity and all other workspace tests also passed.

| Gate at 9d12e65 | Result |
| --- | --- |
| `cargo test --workspace` | PASS, isolated canonical HOME/state/runtime/temp and private NIX_REMOTE store |
| `cargo test -p mandala-context -- --test-threads=32` | PASS, 70 passes; only documented fork limitation ignored |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| `cargo fmt --all -- --check` | PASS |
| `nix fmt -- --check .` | PASS |
| `nix flake check` on macOS | PASS, two native checks; incompatible architectures omitted normally |
| normal `nix build .#mandala-rs` on macOS | PASS, real rebuild/check phase 7m04s; configured sandbox=false |
| `nix build .#mandala-rs --option sandbox true` on Linux | PASS, real whole-workspace rebuild/check phase 5m05s |

Final package outputs:

- macOS normal: `/nix/store/m0r85dqj0hjiqrkg412b6zv0m5r1rnxn-mandala-0.1.0`.
- Linux sandbox: `/nix/store/7cdx6mcs2w324l66cnjkhdqckfprkbpf-mandala-0.1.0`.

Logs: `/tmp/lab218-policy9-{build,workspace,context,clippy,rustfmt,nixfmt,flakecheck}-mac.log`, with exit files all zero. Private native Nix store uses canonical `/private/tmp/...`, not symlink `/tmp/...`; the initial noncanonical fixture was corrected before the passing runs.

## Darwin sandbox comparison: explicitly not a passing claim

The original explicit sandbox attempt denied loopback bind with EPERM before root/home resolution. The old generic standalone fallback could mask that for MCP initialization, but discovery assertions correctly revealed no context; no full baseline sandbox pass is claimed.

After the approved local-network attribute, context/MCP gates pass. The explicit sandbox build reaches unchanged `mandala-eval-worker/tests/git_reload.rs:76` and fails commit A with two `init failed: Null pointer error` lines and `eval worker unavailable: worker closed stdout`.

The coordinator's exact baseline comparison was executed as a real compile/check, using pinned main `ecbec4da4b4cb8d5581e9da02dc2300c1a4a22ab` and ONLY `__darwinAllowLocalNetworking = true` override. It fails the same case (0 passed/1 failed, 0.30s), proving pre-existing/out of scope. Main was not modified.

- Baseline: `/tmp/lab218-baseline-mac-sandbox.log`, derivation `mvrxxcxlswdglqilksm6m7nvdiwpgrkc-mandala-0.1.0`.
- Branch original comparison: `/tmp/lab218-final-build-mac.log`, derivation `wkcmlsnp13qwbz2l4zzih99r7ivqkg42-mandala-0.1.0`.
- Actual post-fork-amendment branch sandbox rerun at 711a59e: same failure, 0.38s; `/tmp/lab218-amended-sandbox-mac.log`, derivation `zdwmwrkmdwgk1wqxlk08aw313534xnl8-mandala-0.1.0`. Corresponding Linux sandbox rebuild passed in 5m05s.

Under coordinator decision 8, that eval-worker sandbox component is not applicable as this change's merge gate; normal Mac/native workspace plus Linux sandbox are the approved bar. No eval-worker or further sandbox-policy changes were made, and no cache hit is presented as sandbox-rebuild evidence.

## Requirement-to-test mapping

| Delta requirement | Evidence |
| --- | --- |
| One shared execution context per checkout | `promotion_race_has_exactly_one_winner`; deterministic disappearing-occupant serialization (gated guard owner, real fast-attempt hook, panic-on-loser factory); every stress trial's authenticated endpoint/dispatch assertions; actual process-exit factory count exactly one |
| Leader failover without losing running work | idempotent read retries once; mutation failover never replays; post-death call bounded failover; resumed subscriptions; `process_exit_promotes_once_and_retries_only_reads` with 32 sessions/16 read retries/no mutation replay; frontend detached-deploy survival/reattach test |
| Discovery and authorization | stale/missing discovery, walked collisions, token mismatch refresh, busy leader never skipped, bad bearer/hello tests, rotation preserves old authenticated connections and cuts old new-connects, actual bound-port assertions |
| Process lock / fork amendment | distinct-process exclusion/SIGKILL release; independent same-process sessions/canonical aliases/flakes; held pre-exec/failed-exec helper tests; independent-process F_GETLK before/after alias/replacement error; actual-serving safe-busy then exec/exit recovery; production spawn audit/immediate-exec rule; stronger ignored witness explicitly outside the amended guarantee |
| Listener closure / work ownership | unscheduled listener-owner guard test; wire/local/watch future destruction before factory; retained local snapshot rejection; cancellation/publication/factory panic/bind exhaustion; all five blocking_work guards, including runtime loss; precise two-site read-only carve-out with late-result discard; queued registered AND snapshot tests |
| Coordination failures fail closed | same coordination root/different metadata roots gives bounded busy/no factory; root/file unsafe/missing/unwritable cases; default creation and parent failures; filesystem admission/unsupported errno tests; MCP IO/busy still initialize, every one of 19 tools explicit error, no run/audit dispatch, failed IO listener/lease gone before initialize |
| Coordinated upgrades | operational docs require stop all user/host frontends, upgrade every path, restart together; no old-binary fence or mixed-version claim; never unlink lock inodes; separate rollout decision remains 6.3 |
| Test environment | strict preflight/budget/timeout unit test; WSL default-namespace failure/private-netns pass; native Mac pass; immutable copies, isolated roots, loaded repetitions and actual Linux sandbox build |
| Stable private rendezvous inode | stable account-home/override/runtime-absent layout tests; isolated secure default creation; missing injected root never created; owner/mode/type/symlink/FIFO/parent/open-race tests; alias/deferred-close guard with external F_GETLK; persistent inode across recovery/shutdown/rotation; lock contents unchanged and never authority |
| Coordination versus serialized metadata | existing discovery golden bytes and concurrent atomic writers, protocol stable tags, unchanged identity/discovery/protocol serialization source, workspace run/event metadata gates, no lock-file metadata authority or version migration |

## Coverage limits, non-applicable branches and scope

Foreign-owner validation uses injected metadata because a native foreign-owner inode fixture requires privilege; no live chown. Unsupported-lock errno classification is injected, not a claim that an unsupported filesystem was qualified. Default creation uses an isolated account-home fixture; actual account-home resolution tests are read-only and never open/create the live coordination root. Local FS policy is opened-fd Darwin MNT_LOCAL / Linux known-remote-FUSE denylist, with others assumed local per the explicit decision; no network-home qualification.

Factory Result-returning failure and recoverable host-start failure are not applicable to current infallible signatures. Their reachable boundaries are tested by factory/job panic, bind exhaustion, cancellation and discovery publication failure. Running registered jobs can delay exclusion release indefinitely; no rollback of already-launched external effects or cancellation of independent supervisors is claimed.

All commits are signed. Only the approved Cargo dependency edge, Darwin attribute, fork amendment and blocking-work amendment expand the initial artifacts/scope; no other deviation. No CI, flake.lock, canonical-spec, .okf, .serena or other-repository changes. Manual strict OpenSpec validation (CLI unavailable) checks exact modified names, headings and 8 fleet-context requirements/35 WHEN-THEN scenarios plus 2 state-format requirements/11 scenarios. All fresh WSL clones `/tmp/lab218-impl-3`, `-4` and `-5` and their owned copied-binary/root fixtures were removed after clean-status/process checks; final clone 5 was verified at 9d12e65 before deletion. Final CPU load was stopped/reaped. The early failed unwritable-parent fixture required owner-verified directory write-bit restoration only for disposal, without following symlinks or touching live paths. Full signed commit list, final diff stat and clean local status are reported by DM at handoff.
