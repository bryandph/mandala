---
type: Decision
title: Interactive nom and native structured build state serve different consumers
description: Preserve the presentation/model split rather than treating nix-build-forest as superseded by the interactive renderer.
tags: [mandala, nix, build, tui]
status: draft
generated:
  by: worker-mandala-BPH-66-1/openai-gpt-6.1-sol
  at: 2026-11-15T05:38:27Z
sources:
  - id: original-memory
    resource: https://git.bph/bryan/nixspace/src/commit/ae19e6622a977e985b3521f17fec5895cd9fb51d/.serena/memories/mandala/build-renderer-split.md
  - id: cli-contract
    resource: ../openspec/specs/fleet-cli/spec.md
  - id: mcp-contract
    resource: ../openspec/specs/fleet-mcp/spec.md
  - id: nom-host
    resource: ../crates/mandala-tui/src/nom_pane.rs
  - id: native-forest
    resource: ../crates/nix-build-forest/src/lib.rs
  - id: event-routing
    resource: ../crates/mandala-core/src/runner.rs
  - id: public-package
    resource: ../flake.nix
---

# Interactive nom and native structured build state serve different consumers

## Current implementation

The interactive build tab hosts `nix-output-monitor` (`nom --json`) in a
pane-sized PTY in `crates/mandala-tui`. Its stdin is a pipe; stdout/stderr use
the PTY. The native `crates/nix-build-forest` model remains in use for headless
progress/final summaries and MCP structured build detail. It exposes bounded
activity, recent per-derivation logs, transfers and elision information; it
is not dead code replaced by `nom`.[^nom][^native][^headless][^mcp]

The behavior contracts are [Native build-forest rendering][cli-contract] and
[Structured build progress reporting][mcp-contract]. Those specifications,
not this concept, define the renderer, packaging, lifecycle and status
requirements.

The event tailer routes each retained `nixlog` line to both the native forest
and the presentation sink; the TUI attaches that sink before polling. These
are internal-JSON records, not a model reconstructed by scraping terminal
output. The protocol parser ignores non-`@nix` lines rather than interpreting
old human-readable Nix progress. Ordinary stderr can still be displayed as
text; that is not a second build-progress parser.[^routing][^attach][^parser][^headless]

Do not interpret “same verbatim stream” as a lossless archive of every Nix
message: the engine feeds its native forest directly, while persisted
`build.jsonl` records are filtered and capped before replay. The retained
`nixlog` payload is passed unchanged by the tailer, but its input is a bounded
subset.[^headless][^persistence]

The public package's `wrapProgram` prefixes `PATH` with
`pkgs.nix-output-monitor`. Any packaging of Mandala needs that wrapped runtime
`PATH`, not merely an ambient development-shell tool, to satisfy the linked
packaging contract. If `nom` cannot start, the pane reports a non-fatal notice
and the summary continues tracking the build. The implementation and tests
also cover early input, attached-run replay, EOF/final-summary handling and
resize propagation.[^package][^nom][^attach][^pane-tests][^attach-test]

## Historical rationale

The original memory attributes the native interactive renderer to
**nixspace#98** and the return to `nom` to **nixspace#105**, settled on
2026-08-15. It reports that native presentation was worse on the full nixspace
fleet and that maintaining another interactive renderer did not justify the
UX gap and complexity. This is attributed history, not a current benchmark
or a new behavior contract. Its useful caution remains: a proposal to replace
`nom` needs new fleet-scale presentation evidence, while removing the native
model would also remove headless and MCP functionality.[^original]

## Migration decisions

Checked against Mandala `0ddbf438258725739a21f32ecfa559d057207827`.

| Original claim | Disposition and reason |
| --- | --- |
| Interactive `nom --json` in a PTY; native headless/MCP state | **Kept.** Both implementations and their consumers remain present.[^nom][^native][^headless][^mcp] |
| Bounded projection, owned recent logs, transfers and elision | **Kept.** These are native summary fields, with MCP detail opt-in rather than an unbounded default response.[^summary][^mcp] |
| Same verbatim stream for both renderers | **Corrected.** Tailer fan-out preserves retained payloads, but build persistence filters/caps records; do not promise complete replay.[^routing][^persistence] |
| Internal-JSON-only parsing “anywhere” | **Corrected in scope.** Build-progress parsing uses internal JSON; ordinary stderr is still shown as text. No human-log progress fallback is present on this path.[^parser][^headless] |
| Native interactive renderer reversal and UX rationale (#98/#105) | **Kept as attributed history only.** No claim that the old comparison is a current benchmark.[^original] |
| Both public and operator flakes wrap `nom` | **Corrected to the public package and general packaging rule.** Dropped the specific nixspace operator-flake assertion from Mandala-owned guidance; downstream packaging belongs to that repository.[^package][cli-contract] |
| Non-fatal fallback, attach/replay, EOF and resize | **Kept as implementation pointers.** The existing OpenSpec contract remains authoritative.[^nom][^attach][^pane-tests][^attach-test] |
| Related `mem:` links | **Dropped** the unresolved research-memory link; **replaced** the reload-memory link with the [bundle concept](/evaluator-reload-freshness.md). |

[^nom]: ../crates/mandala-tui/src/nom_pane.rs
[^native]: ../crates/nix-build-forest/src/lib.rs
[^headless]: ../crates/mandala-core/src/engines/deploy.rs
[^mcp]: ../crates/mandala-mcp/src/server.rs
[^summary]: ../crates/nix-build-forest/src/forest.rs
[^routing]: ../crates/mandala-core/src/runner.rs
[^attach]: ../crates/mandala-tui/src/deploy.rs
[^parser]: ../crates/nix-build-forest/src/msg.rs
[^persistence]: ../crates/mandala-core/src/build_log.rs "Filtering policy; EventWriter in runner.rs applies filtering and the byte budget"
[^package]: ../flake.nix
[^pane-tests]: ../crates/mandala-tui/tests/nom_pane.rs
[^attach-test]: ../crates/mandala-tui/tests/deploy_flow.rs
[^original]: https://git.bph/bryan/nixspace/src/commit/ae19e6622a977e985b3521f17fec5895cd9fb51d/.serena/memories/mandala/build-renderer-split.md

[cli-contract]: ../openspec/specs/fleet-cli/spec.md#requirement-native-build-forest-rendering
[mcp-contract]: ../openspec/specs/fleet-mcp/spec.md#requirement-structured-build-progress-reporting

Migrated with current-source checks from the [original memory][original].

[original]: https://git.bph/bryan/nixspace/src/commit/ae19e6622a977e985b3521f17fec5895cd9fb51d/.serena/memories/mandala/build-renderer-split.md
