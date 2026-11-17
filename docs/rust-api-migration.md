# Additive-safe binary launch API (LAB-14)

Mandala's Rust API is source-based (not an ABI promise). Public launcher configs
and requests now prevent exhaustive construction/matching outside their defining
crate. This is a one-time migration: future optional fields or TUI modes need no
operator-binary edits. Removing or renaming existing API is still breaking.

## Operator `main.rs` replacement

Keep supervisor capture, MCP wiring, all engine registrations and `.run()` as
before. Replace the entire `.tui_launcher(|flake, request| { match request { ... }
})` closure with this exact line:

```rust,ignore
        .tui_launcher(mandala_tui::launch_blocking)
```

Remove these now-unused imports (if grouped with other imports, retain the others):

```rust,ignore
use mandala_core::TuiRequest;
use mandala_tui::deploy::{DeployConfig, run_attach_blocking, run_deploy_blocking};
use mandala_tui::explorer::{ExplorerConfig, run_explorer_blocking};
```

If `Cli` and `TuiRequest` were imported together, replace that import with:

```rust,ignore
use mandala_core::Cli;
```

For example, the public binary's entire assembly is now:

```rust,ignore
use std::process::ExitCode;
use mandala_core::Cli;
use mandala_core::engines::{ansible, deploy};

fn main() -> ExitCode {
    // SAFETY: first operation at executable entry, before threads/env readers.
    unsafe { mandala_core::supervisor::capture_at_entry() };
    Cli::new()
        .mcp_launcher(|flake| match mandala_mcp::serve_stdio_blocking(flake) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("mandala mcp: {e}");
                ExitCode::FAILURE
            }
        })
        .tui_launcher(mandala_tui::launch_blocking)
        .register(deploy::engine())
        .register(ansible::engine())
        .run()
}
```

The downstream operator retains its additional `.register(Engine::new(...))`
lines verbatim. The dispatcher owns all explorer/deploy/attach config plumbing,
error prefixes, run return-code conversion, and bounded runtime shutdown through
the existing blocking entries. It does not change flags, defaults or behavior.

## Direct screen users and custom launchers

Replace a `DeployConfig` literal with construction plus only the overrides the
caller actually needs. The equivalent of the old eight-field literal is:

```rust
use mandala_tui::deploy::DeployConfig;
# let (flake, limit) = (".", "web");
# let (dry_activate, boot, halt_on_build_failure, throttle, cache_push) = (false, false, false, 4, None);
let mut cfg = DeployConfig::new(flake, limit);
cfg.dry_activate = dry_activate;
cfg.boot = boot;
cfg.halt_on_build_failure = halt_on_build_failure;
cfg.throttle = throttle;
cfg.cache_push = cache_push;
// program already defaults to None.
```

`ExplorerConfig::for_flake(flake)` followed by `cfg.debug_mcp = debug_mcp`
remains valid without changes. `ExplorerConfig::default()` is also supported.
Do not use struct update literals (`ExplorerConfig { ..Default::default() }`):
`#[non_exhaustive]` prohibits those too in external crates.

Create requests with `TuiRequest::explorer(debug_mcp)`,
`TuiRequest::deploy(limit)` (CLI defaults), or `TuiRequest::attach(run_id)`.
A custom launcher may match known modes with `..` in each struct pattern and
must include an enum-level `_` fallback. Optional deploy fields can be changed
through a mutable `TuiRequest::Deploy { cache_push, .. }` pattern. Prefer the
shared dispatcher: hand-written matches still need updating to *support* future
options/modes, even though their catch-all patterns keep compiling.

## Audit boundary

Changed:

- `mandala_core::TuiRequest`: enum and all three struct-like variants are
  non-exhaustive; added `explorer`, `deploy`, and `attach` constructors.
- `mandala_tui::deploy::DeployConfig`: non-exhaustive; added `new(flake, limit)`
  with the existing CLI defaults (throttle 4; booleans false; cache override and
  test program absent).
- `mandala_tui::explorer::ExplorerConfig`: non-exhaustive; existing `for_flake`
  and `Default` already provide supported construction.

Audited and left alone:

- `Cli`, `Engine`, `Inventory`, and `Evaluator` already hide construction fields
  behind constructors/loaders. An engine handler accepts a borrowed `Inventory`;
  downstream registration does not construct its representation.
- The supervisor capture guard is opaque; MCP's `serve_stdio_blocking` takes a
  flake string, not a public config. `MandalaHandler` and `ContextHandler` have
  private state and constructors, not all-public launcher configs.
- `HostConfig` (already has `new`), `ContextIdentity`, discovery/protocol types,
  and MCP effects/tool DTOs belong to the context/server/protocol APIs, not the
  binary's Cli/launcher/engine-registration seam. No blanket data-model change.
- Inventory `Aggregate`/`Member`, runner/deploy plan/result types, campaign and VM
  request/data types, `cache_publish::Config` (private fields), and native deploy
  `CmdOverrides`/`DeployData`/`DeployDefs` are engine-domain/data APIs, not configs
  passed by a downstream binary during registration or TUI launch.
- TUI app/state/event/theme/widget structs are runtime/rendering APIs, not bin
  launcher inputs. `DeployJob` additionally already has private fields and `new`.

This migration intentionally does not claim every public Rust domain model is
additive-safe. Those independent APIs and their serialized contracts are outside
the operator-binary seam fixed here.

`crates/mandala-tui/tests/public_api.rs` compiles as an external crate and checks
constructor defaults, mutable overrides, open request patterns, shared dispatcher
registration, and a non-terminal attach refusal. Core rustdoc compile-fail cases
also enforce the new construction and exhaustiveness restrictions.
