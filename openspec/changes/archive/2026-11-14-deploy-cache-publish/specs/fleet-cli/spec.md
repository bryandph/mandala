## ADDED Requirements

### Requirement: Cache publish selection is consistent across native launch surfaces
Headless `mandala deploy run` and standalone `mandala tui deploy` SHALL accept mutually exclusive `--cache-push` and `--no-cache-push` options. The explorer deploy confirmation SHALL expose auto/on/off selection, defaulting to auto each time it opens. MCP `deploy` SHALL accept optional `cache_push` boolean intent: true enables, false disables, and absence preserves auto detection. Every launcher SHALL preserve this tri-state through to the engine without reading credentials or bypassing existing confirmation and wrapper gates. The engine SHALL resolve the environment before run registration and build effects under the `fleet-deploy` cache publish selection contract.

#### Scenario: omission does not silently disable configured publishing
- **WHEN** CLI, TUI or MCP launches a native deploy without an explicit publish override
- **THEN** the engine applies its environment-based auto detection rather than receiving false

#### Scenario: explicit disable overrides configuration
- **WHEN** any native launch surface requests cache publishing off
- **THEN** the engine skips publication regardless of its configured upload environment

#### Scenario: explicit enable needs a destination
- **WHEN** a launch surface requests cache publishing on without a nonempty cache URL
- **THEN** the engine reports a preflight error before run registration or build effects

#### Scenario: contradictory CLI options are a usage error
- **WHEN** either deploy command receives both publish override flags in either order
- **THEN** it rejects the invocation instead of selecting the last flag

#### Scenario: explorer choice resets
- **WHEN** the operator changes the publish choice, dismisses confirmation, and opens a new deploy confirmation
- **THEN** the new dialog starts in auto mode and still requires explicit confirmation to launch

### Requirement: Cache publish observation is shared and advisory
Headless output, the TUI runs list, owned and attached deploy views, and MCP `deploy_status` SHALL expose the shared publish phase and its safe outcome. Active publication SHALL appear as `cache-publish`, independently of host concurrency. Observers SHALL show only the redacted cache destination, classified failure reason and process result when available; raw upload output and netrc paths SHALL NOT be exposed. The settled TUI summary and default curated MCP metadata SHALL retain advisory publish warnings without requiring raw diagnostics. Publish failure SHALL NOT add a fleet host, change host totals, convert a successful deployment into failure, or claim host activation has completed. Build and sticky host failures SHALL retain their existing result precedence.

#### Scenario: attaching during publication
- **WHEN** a frontend attaches to a run after build completion while the shared upload is active
- **THEN** it reconstructs cache-publish from the registry and shows the safe destination without claiming fan-out has started

#### Scenario: observing warning and successful fallback
- **WHEN** publication fails ordinarily but the existing per-host copy and deployment succeed
- **THEN** the TUI summary and MCP status show the publish warning alongside the successful effective run result

#### Scenario: publish is not another fleet member
- **WHEN** an observer reads shared controller publish records and eligible-host publish milestones
- **THEN** it keeps the original host count and treats publish milestones as non-terminal attribution of one upload

#### Scenario: legacy record remains readable
- **WHEN** an observer attaches to a run written before publish metadata existed
- **THEN** it renders the supported run and host state without inventing a publish phase or requiring migration
