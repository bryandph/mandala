## ADDED Requirements

### Requirement: Campaign state survives context reload and replacement
Campaign targets SHALL be durable and independent of the warm latest-inventory evaluator. Reload and checkout watching MAY replace the shared latest inventory but SHALL NOT rebind an existing campaign, replace its intended paths or clear its exclusions/outcomes. Campaign capture SHALL use a fresh or immutable-content-keyed evaluator over the frozen source graph. A replacement context SHALL reconcile registered supervisor-owned attempts without repeating their mutations.

#### Scenario: reload observes a new source
- **WHEN** the context reloads after a checkout or nested input change
- **THEN** latest inventory reflects the reload while existing campaigns retain their original source, target and exclusions

#### Scenario: leader disappears during campaign execution
- **WHEN** a campaign attempt survives its launching context
- **THEN** a replacement context discovers the same run and target and does not activate hosts again because the original response was lost

### Requirement: Campaign mutation is serialized across frontends
Leader-served, follower-proxied and standalone campaign operations SHALL share per-campaign interprocess locking, generation checks, eligibility and confirmation policy. At most one mutating continuation SHALL be active per campaign. Dispatch, exclusion updates and close SHALL be serialized; updates/close SHALL refuse a live or uncertain mutation. Locks SHALL NOT be held across interactive review; dispatch SHALL revalidate the reviewed generation. Existing activity, audit and no-auto-retry mutation rules SHALL apply.

#### Scenario: two frontends retry simultaneously
- **WHEN** a CLI and MCP client attempt to dispatch the same eligible campaign members
- **THEN** only one continuation launches and the other returns a conflict/busy result without duplicate host work

#### Scenario: an exclusion update follows interactive review
- **WHEN** policy changes while a TUI confirmation dialog is open
- **THEN** the reviewed generation is stale and the action requires new confirmation instead of using changed eligibility
