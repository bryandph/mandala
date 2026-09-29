# fleet-state-formats Specification

## Purpose
The on-disk state-directory formats (run registry, deploy metadata, drift snapshots) as a versioned protocol shared by every mandala frontend, with stability gated by in-tree serialization tests.

## Requirements
### Requirement: State-dir formats are a versioned multi-frontend protocol
The on-disk formats under the mandala state directory SHALL be treated as a
compatibility contract shared by every fleet frontend (CLI, TUI, MCP server),
independent of implementation language: the run-registry layout
(`runs/<run-id>/` with `meta.json` and per-plugin `*.jsonl` event streams plus
`output.log` for command runs), the JSONL event protocol (versions 1 and 2,
gated by the `v` field), drift snapshots (`<host>.json` keyed by filename),
the rev-keyed expected cache (`.expected.json`), the per-context discovery
files (under `mcp/`, mode 0600, keyed by canonical checkout, recording
endpoint address, bearer token, leader pid, and flake path — succeeding the
single `mcp/session.json`), and the MCP audit log (`mcp/audit.jsonl`).
Any implementation SHALL ignore event records whose version it does not
support rather than failing, and SHALL treat unknown fields as forward
compatibility, not errors.

#### Scenario: an unsupported event version is skipped
- **WHEN** a frontend tails an event stream containing a record with an
  unrecognized `v` value
- **THEN** the record is skipped and later supported records are still
  consumed

#### Scenario: formats survive an implementation swap
- **WHEN** the porcelain implementation changes (e.g. Python to Rust)
- **THEN** existing registry runs, snapshots, caches, and discovery files
  remain readable without migration, and files it writes remain readable by
  the prior implementation

### Requirement: Format stability gated by in-tree serialization tests
The state-dir formats SHALL remain byte-stable, gated by tests that carry
their expected bytes and judgements in-tree: the canonical serializations
(`meta.json` and `.expected.json` 1-space sorted JSON, the context
discovery files, event JSONL shapes) SHALL be asserted against inline
golden bytes, and reader judgements (drift statuses, liveness, sticky host
states, unknown-event-version skipping) SHALL be pinned by behavior tests.
Registry writes SHALL be atomic (write-then-rename for `meta.json`) and
tailers SHALL tolerate partial trailing lines by re-reading them on the
next poll. A change to any format SHALL fail these gates before it ships;
a deliberate contract change requires a reviewed test-expectation edit and
a version bump where the format is versioned.

#### Scenario: a writer drifts from the golden bytes
- **WHEN** a serialization change alters the bytes written for a canonical
  case
- **THEN** an in-tree golden-byte test fails before the change ships

#### Scenario: a reader drifts from the pinned judgements
- **WHEN** a reader change alters the drift status, liveness, or host-state
  judgement for a pinned case
- **THEN** the behavior test fails, distinguishing a deliberate contract
  change (expectations edited, reviewed) from an accidental regression

### Requirement: Deploy metadata persists per-run boot intent
Every native deploy run SHALL write an additive `boot` boolean to its registry
metadata. Readers SHALL reconstruct absent `boot` fields from older records as
false, and status projections SHALL retain the field wherever run options are
reported.

#### Scenario: new boot run is portable across frontends
- **WHEN** any frontend launches a deploy with the boot override
- **THEN** `meta.json` records `boot: true` and another frontend can attach and
  reconstruct the same run intent

#### Scenario: old record remains readable
- **WHEN** a frontend attaches to deploy metadata written before the `boot`
  field existed
- **THEN** it treats boot override as false without failing or changing the
  record's other judgements

### Requirement: Reboot-pending is an additive successful deploy state
The deploy event protocol SHALL recognize `reboot-pending` as a sticky terminal host state meaning that the expected generation was installed as the next boot default without live activation. Run metadata SHALL add a `reboot_pending` summary count. `confirmed` and `reboot-pending` SHALL contribute to successful settlement, while `failed` and `rolled-back` retain their existing failure precedence. Readers that do not recognize the new milestone or additive summary field SHALL continue processing supported events and fields.

#### Scenario: staged host settles successfully
- **WHEN** a host event stream reaches the reboot-pending milestone and later records status done with rc zero
- **THEN** updated readers retain reboot-pending as the host's successful terminal state

#### Scenario: rollback still determines failure
- **WHEN** one host is reboot-pending and a sibling reaches rolled-back
- **THEN** the first host remains reboot-pending and the overall run is unsuccessful because rollback retains failure precedence

#### Scenario: older reader ignores the new milestone
- **WHEN** a prior reader encounters the additive reboot-pending milestone or summary field
- **THEN** it ignores the unknown data rather than rejecting the remaining run record

### Requirement: State snapshots record the installed system profile additively
Each new fleet state snapshot SHALL include `system_profile`, the resolved installed system-profile target that supplies the next-boot generation. Readers SHALL tolerate snapshots where the field is absent and MUST treat it as observational state rather than proof of live activation.

#### Scenario: new snapshot records a staged generation
- **WHEN** the system profile points at a generation that differs from `/run/current-system`
- **THEN** the snapshot records both paths without claiming that the system-profile generation is running

#### Scenario: legacy snapshot remains readable
- **WHEN** a reader loads a snapshot written before `system_profile` existed
- **THEN** it applies the prior current/booted judgement without a parse or migration failure

### Requirement: Reboot-pending format judgements are regression gated
Canonical serialization and reader-behavior tests SHALL cover the additive `system_profile` snapshot field, reboot-pending milestone and terminal state, `reboot_pending` run-summary count, successful mixed confirmed/reboot-pending settlement, and failure precedence when failed or rolled-back hosts are present.

#### Scenario: a reboot-pending judgement drifts
- **WHEN** an implementation stops treating reboot-pending as successful and terminal or loses the staged system-profile judgement
- **THEN** an in-tree serialization or behavior gate fails before release
