## ADDED Requirements

### Requirement: Strict serializable org schema

`lib.contract.evalOrg` SHALL validate a non-empty name, non-empty domain list, and the optional `mirrors` and `sites` sections. It SHALL return JSON-compatible data including contract version 1 and versions of present sections. Unknown structural keys SHALL fail. Roster and identity fields SHALL NOT be part of this interface.

#### Scenario: Minimal org
- **WHEN** globex supplies only a name and domain list
- **THEN** evaluation SHALL succeed with mirrors and sites absent

#### Scenario: Invalid core
- **WHEN** data contains an unknown key or empty required core value
- **THEN** full validation SHALL fail naming the field

### Requirement: Versioned Nix mirror catalog

The optional mirrors section SHALL contain version 1, at most 30 named entries, and an ordered default selection. Entries SHALL contain an HTTP(S) URL without credentials, query, or fragment; a non-empty list of syntactically valid Nix public keys; and optional string labels. All references SHALL name existing entries; selection lists SHALL reject duplicates. Unsupported section versions SHALL fail with named errors.

#### Scenario: Too many entries
- **WHEN** the mirrors section declares 31 entries
- **THEN** validation SHALL fail naming the section and the 30-entry limit

#### Scenario: Invalid mirror reference
- **WHEN** a default or site selection refers to a missing entry
- **THEN** validation SHALL fail naming the selection and missing entry

#### Scenario: Unsupported mirrors version
- **WHEN** the section declares a version other than 1
- **THEN** validation SHALL fail naming the section and supported version

#### Scenario: Invalid mirror entry
- **WHEN** an entry has a credential-bearing URL, a reserved URL component, empty keys, or a malformed key
- **THEN** validation SHALL fail naming the entry and field

### Requirement: Org-level site catalog

The optional `sites` section SHALL be version 1 and SHALL be keyed by site codes matching `^[A-Z0-9-]+$`, independent of the mirrors section. In this slice a site SHALL accept only `mirrors.preferred`, an ordered, duplicate-free list of mirror entry names; a non-empty list SHALL require the mirrors section and existing entries. Later sections SHALL add per-site settings beside `mirrors` under the same site code.

#### Scenario: Site without mirror preferences
- **WHEN** an org declares site `ALPHA` with no `mirrors` key and no mirrors section
- **THEN** evaluation SHALL succeed and selecting `ALPHA` SHALL be valid

#### Scenario: Preference without mirrors section
- **WHEN** a site lists preferred mirror entries but the org has no mirrors section
- **THEN** validation SHALL fail naming the site and the missing section

### Requirement: Deterministic site selection

Effective mirrors SHALL be the selected site's preferred entries followed by defaults, deduplicated preserving first occurrence. Selecting a site not declared in `sites` SHALL fail. The selected entries alone SHALL supply adapter URLs and signing keys.

#### Scenario: Site preference and fallback
- **WHEN** a site's preferred list overlaps the default list
- **THEN** its entries SHALL appear first and repeated entries SHALL appear only once

#### Scenario: Absent section
- **WHEN** mirrors is absent, with no site or a declared site selected
- **THEN** the adapter SHALL contribute no configuration

#### Scenario: Unknown site
- **WHEN** a site is selected that is not declared in `sites`, including when mirrors or sites is absent
- **THEN** validation SHALL fail naming the site

### Requirement: NixOS daemon adapter

The adapter SHALL accept evaluated org data and an optional site through module options. It SHALL add selected caches to daemon `nix.settings.substituters` with explicit priorities `10 + index`, which the 30-entry cap bounds to 10–39, ahead of the default priority 40, and add their keys to `trusted-public-keys`, preserving host defaults through additive definitions. It SHALL NOT grant trusted-user privileges. Documentation SHALL explain that host installation authorizes the selected keys globally for Nix store signature trust.

#### Scenario: Effective configuration
- **WHEN** a NixOS configuration selects an acme site
- **THEN** effective settings SHALL include correctly prioritized site and default mirrors, selected keys, and existing host defaults

### Requirement: Runtime substitution proof

A NixOS VM test SHALL exercise signed fictional caches through the daemon with an unprivileged client. Each fixture cache SHALL be served by its own HTTP server with request logging, because Nix does not report which substituter served a path. Test-generated keys SHALL stay within test execution. Tests SHALL prevent local builds or other caches from masking substitution failures.

#### Scenario: Preferred cache serves the object
- **WHEN** both fixture caches offer the same missing store object and the host installs the adapter
- **THEN** an unprivileged client's request SHALL substitute successfully, the preferred cache's request log SHALL show the object's `.narinfo` and `.nar` requests, and the other cache's log SHALL show no `.nar` request for it

#### Scenario: Missing daemon trust
- **WHEN** an unprivileged client requests an object available only from a cache/key not authorized by the daemon
- **THEN** substitution SHALL fail without silently building the object or granting trust
