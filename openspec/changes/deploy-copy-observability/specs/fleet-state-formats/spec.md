## ADDED Requirements

### Requirement: Copy telemetry is additive per-host state
Host streams SHALL support additive `copy` events with `substitute_on_destination`, optional `duration_ms`, optional `success`, optional `paths_copied` and optional `controller_nar_bytes`. Start events SHALL omit completion fields. Counts SHALL be unsigned Nix-reported completed paths and cumulative uncompressed NAR payload bytes, not closure estimates or wire bytes. Registry, CLI status, TUI and MCP status SHALL expose the latest optional per-host copy projection. Existing event versions and sticky host outcomes SHALL remain unchanged.

#### Scenario: reattach after completion
- **WHEN** a reader opens a completed run
- **THEN** host events reconstruct mode, duration and available counts without another Nix invocation

#### Scenario: legacy host stream
- **WHEN** older records contain only copy milestones
- **THEN** copy measurements are absent and existing host-state judgement is unchanged

#### Scenario: explicit empty progress
- **WHEN** Nix explicitly reports zero completed paths or bytes
- **THEN** those reported zero values are preserved
