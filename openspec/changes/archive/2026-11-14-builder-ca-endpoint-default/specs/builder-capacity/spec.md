## ADDED Requirements

### Requirement: Omitted CA principals use the effective endpoint name

A CA trust declaration MAY omit principal. Projection SHALL resolve it to the
effective route hostname after route, builder-default and member-FQDN precedence.
Explicit principals SHALL win. The name SHALL NOT include a port or come from
a generated SSH alias or jump endpoint. This applies equally to final and
jump-only routes. Missing or unsafe endpoints SHALL fail evaluation rather than
relaxing strict host checking. Pin behavior SHALL remain unchanged.

#### Scenario: A short name is dialled
- **WHEN** a route dials build-short and declares a CA without a principal
- **THEN** its effective principal and HostKeyAlias are build-short and knownHosts trusts only that name (port-qualified when necessary)

#### Scenario: An explicit principal differs
- **WHEN** a route dials a short name but supplies a CA principal builder.acme.example
- **THEN** certificate verification uses builder.acme.example

#### Scenario: No endpoint can be derived
- **WHEN** a CA route has neither a route hostname, builder hostname nor member FQDN
- **THEN** projection fails closed, without runtime discovery

#### Scenario: Pins remain strict
- **WHEN** a builder uses a public-key pin
- **THEN** it retains its distinct member host-key alias and full-key encoding, and cannot declare a principal
