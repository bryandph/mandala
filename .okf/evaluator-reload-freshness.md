---
type: Gotcha
title: Checkout reload freshness requires a new evaluator worker
description: Replacing warm Nix state and adopting the leader's inventory are separate operations; observers must not recursively reload.
tags: [mandala, nix, evaluation, reload, context]
status: draft
generated:
  by: worker-mandala-BPH-66-1/openai-gpt-6.1-sol
  at: 2026-11-15T05:38:27Z
sources:
  - id: original-memory
    resource: https://git.bph/bryan/nixspace/src/commit/ae19e6622a977e985b3521f17fec5895cd9fb51d/.serena/memories/mandala/evaluator-reload-freshness.md
  - id: evaluator
    resource: ../crates/mandala-core/src/eval.rs
  - id: git-reload-regression
    resource: ../crates/mandala-eval-worker/tests/git_reload.rs
  - id: cli-contract
    resource: ../openspec/specs/fleet-cli/spec.md
  - id: context-contract
    resource: ../openspec/specs/fleet-context/spec.md
  - id: mcp-contract
    resource: ../openspec/specs/fleet-mcp/spec.md
---

# Checkout reload freshness requires a new evaluator worker

## Why clearing the flake cache is not enough

The long-lived `mandala-eval-worker` retains both a Nix `EvalState` and a map
of `LockedFlake` values. Clearing only the map does not remove values memoized
by `EvalState` from a previous checkout. `Evaluator::reload` in
`crates/mandala-core/src/eval.rs` takes and drops the worker; `Worker::drop`
kills and reaps it. The next evaluation lazily starts a new worker and Nix
state. The subprocess backend's reload is a no-op because each call is
already cold.[^worker][^eval]

The real-worker regression `reload_observes_a_moved_git_flake` evaluates
marker A, commits marker B in the same Git checkout, reloads, and verifies B.
It is a live libnix integration test, not just a mocked cache test.[^regression]
The behavior contract is [Reload invalidates all evaluator state][reload-contract];
its successful inventory swap and failed-evaluation behavior belong there,
not in a parallel contract in this concept.

## Where automatic reload ownership lives

`repo_head` and `spawn_repo_head_watch` in `mandala-core/src/drift.rs` watch
commit identity without dirty-state decoration. In
`mandala-mcp/src/context.rs`, the leader-only host-config factory installs the
shared watcher, which dispatches the normal `reload` tool through a weak
handler reference. A context-free explorer takes the fallback watcher path
in `mandala-tui/src/explorer.rs`, capturing its baseline before the initial
evaluation.[^watch][^leader][^explorer]

A reload settlement already represents the leader's evaluation. The TUI's
`ReloadLanded` handler uses `request_inventory_swap`, whose non-fresh load
adopts the shared inventory without calling `reload` again. It also preserves
an independently queued fresh reload if events race. This distinction avoids
one extra cold evaluation per observer.[^adoption][^state]

The authoritative contracts are [Automatic local contract reload][cli-contract]
and [Leader-owned checkout watching][context-contract]. The cross-crate
`context_flow.rs` tests exercise standalone refresh, observer adoption with
one leader evaluation, dirty-edit suppression, leadership promotion and
shutdown. They complement, rather than replace, the real-worker freshness
regression.[^context-tests][^regression]

For the independent presentation/model boundary, see the
[build renderer split](/build-renderer-split.md).

## Migration decisions

Checked against Mandala `0ddbf438258725739a21f32ecfa559d057207827`.

| Original claim | Disposition and reason |
| --- | --- |
| Warm `EvalState` outlives the `LockedFlake` cache | **Kept.** Worker owns both; evaluator documentation explains why cache-only invalidation is insufficient.[^worker][^eval] |
| `Evaluator::reload` drops/reaps the worker; next call starts fresh | **Kept**, with the subprocess-backend no-op made explicit.[^eval] |
| Real moved-Git-flake regression in `git_reload.rs` | **Kept.** It evaluates A, commits B, reloads and asserts B using the worker backend.[^regression] |
| Commit-only helper, leader-owned watcher, context-free fallback | **Kept.** These paths remain in drift.rs, context.rs and explorer.rs.[^watch][^leader][^explorer] |
| Observers adopt through `request_inventory_swap`, not another fresh reload | **Kept**, with the queued-fresh-request race caveat from the implementation.[^adoption][^state] |
| Leadership, promotion, shutdown, dirty-edit and one-evaluation tests | **Kept.** Current cross-crate tests cover those cases; freshness itself is also checked by the separate real-worker regression.[^context-tests][^regression] |
| Track `nixspace#116` and change `mandala-auto-reload-on-commit` | **Corrected.** These names are historical provenance from the original memory. That change directory is absent from both `openspec/changes/` and `openspec/changes/archive/` at the checked commit; link to the current CLI, context and MCP specs instead.[^original][^cli-spec][^context-spec][^mcp-spec] |

The **nixspace#116** attribution is implementation/spec history, not an active
work item or proof of current behavior. Likewise, the historical OpenSpec
change name is not a resolvable local change link at this revision.[^original]

[^worker]: ../crates/mandala-eval-worker/src/nix.rs
[^eval]: ../crates/mandala-core/src/eval.rs
[^regression]: ../crates/mandala-eval-worker/tests/git_reload.rs
[^watch]: ../crates/mandala-core/src/drift.rs
[^leader]: ../crates/mandala-mcp/src/context.rs
[^explorer]: ../crates/mandala-tui/src/explorer.rs
[^adoption]: ../crates/mandala-tui/src/app.rs
[^state]: ../crates/mandala-tui/src/state.rs
[^context-tests]: ../crates/mandala-tui/tests/context_flow.rs
[^cli-spec]: ../openspec/specs/fleet-cli/spec.md
[^context-spec]: ../openspec/specs/fleet-context/spec.md
[^mcp-spec]: ../openspec/specs/fleet-mcp/spec.md
[^original]: https://git.bph/bryan/nixspace/src/commit/ae19e6622a977e985b3521f17fec5895cd9fb51d/.serena/memories/mandala/evaluator-reload-freshness.md

[reload-contract]: ../openspec/specs/fleet-mcp/spec.md#requirement-reload-invalidates-all-evaluator-state
[cli-contract]: ../openspec/specs/fleet-cli/spec.md#requirement-automatic-local-contract-reload
[context-contract]: ../openspec/specs/fleet-context/spec.md#requirement-leader-owned-checkout-watching

Migrated with current-source checks from the [original memory][original].

[original]: https://git.bph/bryan/nixspace/src/commit/ae19e6622a977e985b3521f17fec5895cd9fb51d/.serena/memories/mandala/evaluator-reload-freshness.md
