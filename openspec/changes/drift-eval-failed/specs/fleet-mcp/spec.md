## MODIFIED Requirements

### Requirement: Drift reporting
The server SHALL expose deployed-generation drift as the same
`DriftStatus`/`DriftEntry` judgement the dashboard uses, including
`reboot-pending` when the installed system profile equals the expected
generation while the running generation differs. Exact-path equality and
distinct stale, incomplete, never-surveyed, unreachable, and eval-failed
statuses SHALL remain shared with the CLI/TUI core. When an opted-in
evaluation fails for some members, the tool SHALL report `ok: false`, list
the failed members, carry each one's error on its entry, and MUST NOT cache
the partial expectation. The expensive inputs — the read-only
state survey and expected-toplevel evaluation — SHALL run only when the
request explicitly opts into them; a plain drift read uses existing snapshots
and the rev-keyed expectation cache without surveying or re-evaluating. A
requested refresh that exits non-zero MUST be returned as a failed refresh
with its exit code and captured stdout/stderr; existing snapshots MAY be
included for diagnosis but MUST NOT be labeled freshly refreshed.

#### Scenario: a plain drift read is side-effect-free
- **WHEN** a client reads drift without requesting a refresh or eval
- **THEN** the server reports drift from existing snapshots and the cached expectation without surveying or re-evaluating

#### Scenario: staged expected generation is reported
- **WHEN** an existing snapshot records expected as the system profile but not the running generation
- **THEN** the server returns reboot-pending, matching the dashboard

#### Scenario: opting into a refresh runs the read-only survey
- **WHEN** a client requests refresh and the survey succeeds
- **THEN** the returned judgement incorporates the freshly observed current, booted, and system-profile targets

#### Scenario: a failed refresh preserves diagnostics
- **WHEN** the requested survey exits non-zero
- **THEN** the tool reports failure with the survey exit code and captured diagnostics and does not label existing snapshots freshly refreshed

#### Scenario: drift never redefines the judgement
- **WHEN** a member's snapshot is older than the staleness threshold
- **THEN** the server reports it as stale, matching the dashboard, not as
  in-sync, reboot-pending, or drift

#### Scenario: a partial evaluation is reported per host
- **WHEN** a client opts into evaluation and one member's configuration fails
- **THEN** the tool returns `ok: false` with that member listed as failed, its entry reads eval-failed with its error, the other members are judged normally, and nothing is cached
