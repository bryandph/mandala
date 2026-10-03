## 1. Evaluation

- [x] 1.1 Worker: per-member errors in `expected_toplevels`; whole-flake failures still fail the call; wire value maps members to a path or `{"error": …}`.
- [x] 1.2 Client: `Toplevels { paths, errors }`; parse the worker value; subprocess backend isolates members after a failed batch and returns the batch error when every member fails alone.
- [x] 1.3 Subprocess backend skips members without a `nixosConfigurations` entry.

## 2. Drift judgement

- [x] 2.1 Add `DriftStatus::EvalFailed` (`eval-failed`, style `red`) and `DriftEntry.eval_error` (omitted when absent).
- [x] 2.2 `compare` judges `eval-failed` in place of expectation-dependent statuses; survey-side statuses win.
- [x] 2.3 Pinned judgement test, status/style vocabulary test.

## 3. Frontends

- [x] 3.1 CLI `drift --eval`: render all hosts, list failures on stderr, exit non-zero, do not cache a partial result.
- [x] 3.2 MCP `drift`: `ok: false`, `eval_failed`, no partial cache; `host_eval`: per-member `eval_error`.
- [x] 3.3 TUI: local and context-routed evals carry per-host errors; sticky status names failed hosts; no partial cache.

## 4. Verification

- [x] 4.1 Real-worker integration test (`member_isolation`): a throwing member is isolated, others evaluate, absent members are skipped.
- [x] 4.2 MCP parity case and TUI state test for a partial evaluation.
- [x] 4.3 Workspace clippy (`-D warnings`) and tests; `nix build .#mandala-rs`.
- [x] 4.4 Live: `mandala drift --eval --json` against a real fleet evaluates every host through the worker.
