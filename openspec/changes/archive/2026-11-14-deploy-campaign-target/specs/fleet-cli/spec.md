## ADDED Requirements

### Requirement: Additive CLI and TUI campaign lifecycle
CLI and TUI SHALL expose campaign create, inspect, retry, explicit exclusion updates, reboot, verify and close alongside existing deploy/run interfaces. Real creation SHALL bind the target and start the first deployment attempt in one operation after the same real-mutation confirmation gate as deploy runs. An explicit creation preview SHALL bind and display the target without launching an attempt or performing host effects, leaving real work pending. Retry SHALL execute eligible pending or failed work, including work left pending by a creation preview. Inspection SHALL show source and target identity, source-moved state, membership/exclusions/reasons, outcome provenance, attempts and verification separately from lifecycle. CLI SHALL offer structured reads; TUI SHALL link attempts to existing run attachment. Real mutations SHALL retain explicit confirmation and dispatch revalidation of exact names and campaign generation.

#### Scenario: creation starts the first deployment
- **WHEN** an operator confirms real campaign creation after reviewing the captured target and exact eligible names
- **THEN** the target and first attempt/run link are persisted before host effects and the first deployment starts without a separate retry command

#### Scenario: creation preview preserves target review
- **WHEN** an operator requests a campaign creation preview
- **THEN** the target is bound and displayed without an attempt or host effects and real deployment remains pending

#### Scenario: real creation is not confirmed
- **WHEN** an operator declines or omits the required real-mutation confirmation
- **THEN** no first deployment attempt starts

#### Scenario: legacy run attachment still works
- **WHEN** an operator attaches to an older unlinked run
- **THEN** the existing run interface operates without inventing campaign expectations

#### Scenario: confirmation becomes stale
- **WHEN** campaign policy changes after review but before dispatch
- **THEN** dispatch refuses the stale generation and requires fresh review instead of broadening the confirmed action

### Requirement: Campaign and latest-source drift are separate
Campaign-aware drift views SHALL label current-versus-bound-campaign separately from current-versus-latest-evaluated-source, each with its own expectation provenance and source identity. A global expectation cache refresh SHALL NOT change a campaign target. Snapshot freshness, reachability, completeness, per-host evaluation failures and conservative boot-critical judgments SHALL remain distinct. Missing or unusable expectations SHALL report expectation-unknown, never in-sync/clean, including when expected_source is none. Boot-pending SHALL distinguish installed target from running target and SHALL NOT count as completed live verification.

#### Scenario: latest source diverges after successful deployment
- **WHEN** a fresh snapshot matches the campaign target but not the separately evaluated latest source
- **THEN** the campaign comparison shows its actual completion state and the latest-source comparison reports drift without redefining campaign success

#### Scenario: expectation cache is missing or stale
- **WHEN** no usable per-host latest-source expectation is available
- **THEN** the latest-source comparison is expectation-unknown rather than clean while any valid campaign comparison remains independently available

#### Scenario: staged target is not running
- **WHEN** the installed system profile matches the campaign target but current does not
- **THEN** the campaign view reports reboot-pending and incomplete live verification

#### Scenario: nested identity is unproven by a legacy cache
- **WHEN** only a root-revision cache is available for a content-identified campaign-aware latest-source comparison
- **THEN** it cannot establish source equality and the view reports unknown unless explicit complete evaluation supplies the missing expectation
