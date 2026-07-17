# Interop golden fixtures — the cross-implementation state-dir gate

`state/` is a mandala state directory **written by the Python
implementation through its real code paths** (`registry.write_meta`/
`update_meta`, the collection's `events.Emitter`, `drift.save_expected`)
over fixed deterministic inputs. It is the direction-A half of the
`fleet-state-formats` interop gate (OpenSpec change `mandala-rust-rewrite`,
task 2.5):

- **Rust reads it** in `crates/mandala-core/src/interop_tests.rs`
  (runs in `cargo test` and the `nix build .#mandala-rs` check phase —
  no Python needed at cargo-test time).
- **Python reads the same bytes** in `cli/tests/test_interop_fixtures.py`
  (runs in the `mandala-cli` check phase), asserting the SAME judgements
  — so both implementations are pinned to one verdict over one tree.

Direction B (Rust writes, Python attaches live) is
`cli/tests/test_interop_rs.py` + the `mandala-interop-helper` binary,
wired as the `mandala-interop` flake check.

## Tree contents

Snapshots + `.expected.json` cover every `DriftStatus`: `alpha` in-sync,
`beta` drift, `gamma` reboot-pending (kernel moved), `delta` activated
(quad equal modulo kernel-params whitespace — token normalization),
`epsilon` unreachable, `zeta` incomplete, `theta` stale, `eta` absent
(no-snapshot).

Registry runs (run-id suffix = fake pid; tests fake liveness per pid,
treating `555555` as the one live foreign pid):

| run | kind | exercises |
|-----|------|-----------|
| `…-1001` | deploy | v1 + v2 streams, milestones via real deploy-rs line detection, nixlog, build model, **torn final line** (`beta.jsonl` + its cut-off remainder `beta.jsonl.tail`), liveness running→finished |
| `…-1002` | deploy | rollback-wins-over-confirmed, sticky confirmed vs late `done rc=1`, an unsupported **v99** record skipped with later records still consumed |
| `…-1003` | deploy | batch-build death (build `done rc=2`, no host events) → failed |
| `…-1004` | reboot | command run with reaped `rc=3` + teed `output.log` |
| `…-1005` | build | in-flight command run (no rc) — live-pid pruning + unknown |

## Regenerating

Never hand-edit `state/` — the generator is the provenance:

```
PYTHONPATH=cli/src python3 cli/tests/fixtures/interop/generate.py
```

Regeneration is byte-stable (fixed timestamps/pids/run-ids); if it isn't,
that is itself a finding. If the tree's SHAPE changes, update all three
consuming test files together — they share one expectations table.
