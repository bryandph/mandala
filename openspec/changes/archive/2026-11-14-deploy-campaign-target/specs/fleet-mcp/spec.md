## ADDED Requirements

### Requirement: Additive campaign lifecycle tools
MCP SHALL add campaign_create, campaign_inspect, campaign_retry, campaign_exclude, campaign_reboot, campaign_verify and campaign_close, plus optional campaign_id for drift, using the shared campaign core. Existing tool names, arguments, defaults, run-status budgets and legacy result fields SHALL remain compatible. Non-dry campaign_create SHALL bind the target and start the first deployment attempt in one operation after the same real-mutation confirmation gate as deploy runs. Campaign creation SHALL default to dry_activate true as a target preview: bind and display the target without launching an attempt or performing host effects, leaving real work pending. This creation preview SHALL NOT change campaign_retry's existing dry-activation semantics. Inspection SHALL provide bounded/paginated membership, reasons, target paths and attempt provenance with totals/elision. Campaign identifiers SHALL be validated and confined before filesystem access.

#### Scenario: legacy tool calls are replayed
- **WHEN** a client calls existing deploy_status or deploy without campaign arguments
- **THEN** existing result fields and defaults remain valid and no campaign is implicitly created

#### Scenario: campaign detail exceeds a response budget
- **WHEN** the full host target map exceeds the inspection page size
- **THEN** the response reports totals and continuation/elision explicitly without expanding default run summaries

### Requirement: Campaign mutations retain confirmation and audit
Campaign retry SHALL default to dry activation. Non-dry creation, non-dry retry and every reboot SHALL require confirm equal to the exact resolved eligible comma-joined limit, with source identity and digest/generation revalidated at dispatch. Real creation SHALL persist the target and prepared attempt/run link before host effects. Refusals SHALL return required_confirm, names and generation without launching work. Exclusion updates and close SHALL require explicit generation and confirmation of the campaign id and SHALL be audited with actor/reason. Lifecycle writes SHALL NOT be automatically replayed after an uncertain response or context failover.

#### Scenario: real creation starts its first attempt
- **WHEN** a client requests non-dry campaign_create with matching confirmation for the captured target and exact eligible names
- **THEN** creation binds the target and starts the first deployment attempt without a separate campaign_retry call

#### Scenario: real creation lacks confirmation
- **WHEN** a client requests non-dry campaign_create without matching confirmation
- **THEN** it receives required_confirm, resolved names and generation and no attempt or host work starts

#### Scenario: default creation previews the target
- **WHEN** a client calls campaign_create without overriding dry_activate true
- **THEN** the target is bound and returned for review without an attempt or host effects and real deployment remains pending

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
