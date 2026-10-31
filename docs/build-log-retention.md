# Deploy build log retention

Native deploys write a replayable subset of Nix internal JSON to `build.jsonl`,
not a raw build transcript. The protocol is still append-only v2 JSONL; readers
can keep byte offsets and reconnect without handling rotation or compression.

The writer keeps build/copy/transfer/substitution activity starts and matching
stops, phase/progress/expected and diagnostic results, and error/warning messages.
It drops narinfo queries, unrelated stops, build/post-build log-line results and
informational messages. Ordinary stderr is retained within the byte budget.
There is no raw archive. The CLI's live forest still consumes the original stream.

Each new run's build stream is capped at 16 MiB, including JSON envelopes and
newlines. A 64 KiB reserve allows final aggregate progress and status records even
when detail has exhausted its budget. The writer appends one visible size-cap
notice and then stops recording detail. The final progress record has the optional
`final: true` field (old readers ignore it). After a cap notice, activity graphs
are incomplete: use final aggregate counters and the run outcome, not unfinished
activity rows, to judge completion. Errors before the budget is exhausted survive;
a flood of errors is itself capped. Non-Nix diagnostic strings are shortened to
8 KiB. Existing oversized logs are not rewritten or deleted by this change.

`deploy_status(forest=true)` already omits the full graph. Its forest projection
now also has a 32 KiB serialized byte budget, protecting against unusually long
producer-controlled strings. Above that budget the forest contains only version,
elapsed time, exact counts, completed download/substitution totals and
`detail_truncated: true`; detailed forest lists are absent. Host data and explicit
`forest_nodes=true`/`diagnostics=true` projections have their existing independent
limits; 32 KiB is the forest budget, not a promise for the entire fleet response.

Registered run directories are pruned when allocating a new run. Defaults keep
at most 20 inactive runs, none older than 30 days (age is measured from the run
ID). Configure with `MANDALA_FLEET_RUN_KEEP` (minimum 1) and
`MANDALA_FLEET_RUN_MAX_AGE_DAYS` (0 disables the age limit). A live recorded run PID
or supervisor PID is always exempt and does not count against retention. Before
metadata is published, the allocating PID in the generated run ID protects the
run. Dead allocations without metadata are eligible for pruning. Symlink entries
are not traversed. PID reuse can conservatively retain an old run.

This policy covers `runs/`, not `deploy-launches/`; the latter is separate launch
state tracked by LAB-95. Pruning tests use temporary fixture directories only.

## Consumers and verification

- `mandala-core::runner::EventTailer` globs run JSONLs and feeds retained `nixlog`
  records to `BuildForest`. The synthetic compaction/cap test replays the actual
  output and verifies final error counters, done and return code. Existing native
  engine tests check build/copy/error replay and byte-compatible v2 envelopes.
- `mandala-mcp::server` reads through `ObservedRun`/`EventTailer` for
  `deploy_status`. Existing engine/fixture parity and default projection tests,
  plus the oversized-error forest byte-budget test, cover this path.
- `mandala-tui::deploy` attaches `nixlog_sink` to the same tailer and feeds nom.
  It now shows lifecycle/progress, not per-build output lines. Existing real-nom,
  attach/reconnect, screen and deploy-flow tests remain in the workspace suite.
- Direct JSONL reads in `mandala-core` engine tests and
  `crates/mandala/tests/mcp_stdio.rs` assert native build stream contents. There
  are no other production `build.jsonl` readers in `crates/`.
