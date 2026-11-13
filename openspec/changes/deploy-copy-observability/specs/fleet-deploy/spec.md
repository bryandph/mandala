## ADDED Requirements

### Requirement: Native host copies report measurements
Every attempted native host copy SHALL durably record whether substitute-on-destination is enabled from resolved fastConnection, elapsed copy duration, and Nix-reported completed paths and transferred NAR bytes when available. Reporting SHALL NOT alter copy behavior or defaults. Human logs SHALL remain readable.

#### Scenario: destination substitution enabled
- **WHEN** resolved fastConnection is false
- **THEN** the copy event reports substitute_on_destination true and measurements describe controller-to-destination transfer only

#### Scenario: unavailable measurements
- **WHEN** Nix reports no recognized progress
- **THEN** counts remain absent rather than fabricated zero

#### Scenario: failed copy
- **WHEN** Nix cannot start or returns failure
- **THEN** mode and elapsed duration are recorded with failure and any reported partial counts, without proceeding to activation
