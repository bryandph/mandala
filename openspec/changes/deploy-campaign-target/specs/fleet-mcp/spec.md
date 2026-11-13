## ADDED Requirements

### Requirement: Additive campaign lifecycle tools
MCP SHALL add campaign_create, campaign_inspect, campaign_retry, campaign_exclude, campaign_reboot, campaign_verify and campaign_close, plus optional campaign_id for drift, using the shared campaign core. Existing tool names, arguments, defaults, run-status budgets and legacy result fields SHALL remain compatible. Campaign creation SHALL be bind-only. Inspection SHALL provide bounded/paginated membership, reasons, target paths and attempt provenance with totals/elision. Campaign identifiers SHALL be validated and confined before filesystem access.

#### Scenario: legacy tool calls are replayed
- **WHEN** a client calls existing deploy_status or deploy without campaign arguments
- **THEN** existing result fields and defaults remain valid and no campaign is implicitly created

#### Scenario: campaign detail exceeds a response budget
- **WHEN** the full host target map exceeds the inspection page size
- **THEN** the response reports totals and continuation/elision explicitly without expanding default run summaries

### Requirement: Campaign mutations retain confirmation and audit
Campaign retry SHALL default to dry activation. Non-dry retry and every reboot SHALL require confirm equal to the exact resolved eligible comma-joined limit, with digest/generation revalidated at dispatch. Refusals SHALL return required_confirm, names and generation without launching work. Exclusion updates and close SHALL require explicit generation and confirmation of the campaign id and SHALL be audited with actor/reason. Lifecycle writes SHALL NOT be automatically replayed after an uncertain response or context failover.

#### Scenario: real retry lacks confirmation
- **WHEN** a client requests non-dry campaign retry without matching confirmation
- **THEN** it receives the resolved required_confirm and no run starts

#### Scenario: a preview is not real success
- **WHEN** a default dry campaign retry completes
- **THEN** no live or boot activation occurs and real campaign work remains pending

#### Scenario: policy update races with another update
- **WHEN** a client supplies an outdated campaign generation
- **THEN** it receives a structured conflict and no exclusion or close mutation occurs

### Requirement: Structured drift reports identify both expectation bases
Campaign-aware drift SHALL return independently labeled campaign and latest-source comparisons, source identities, expectation provenance, snapshot timestamps, exclusions and boot-pending state. An absent expectation or expected_source none SHALL NOT produce a clean/in-sync status or verified success. Existing survey and evaluation failures SHALL retain structured diagnostics.

#### Scenario: latest expectation is unavailable
- **WHEN** campaign target data is valid but the latest-source cache is missing or unusable
- **THEN** campaign comparison remains available and latest-source comparison reports expectation-unknown, not success
