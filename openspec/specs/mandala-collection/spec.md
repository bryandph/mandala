# mandala-collection Specification

## Purpose
The `mandala.fleet` Ansible collection's retained read-only deployment-state
survey used by Mandala drift consumers.
## Requirements
### Requirement: Read-only fleet state survey remains available
The `mandala.fleet` collection SHALL retain its `mandala.fleet.state` playbook as a read-only survey over the projected `deploy_rs` inventory group. The survey SHALL record one controller-side deployment-state snapshot per selected host, including the resolved running generation, booted generation, and installed system-profile target used as the next-boot generation. It SHALL treat unreachable members as recorded state rather than a fatal fleet-wide error, and SHALL NOT activate, change a profile, install a boot default, reboot, or otherwise mutate a target host.

#### Scenario: drift refresh surveys without changing hosts
- **WHEN** a CLI, TUI, or MCP drift refresh invokes `mandala.fleet.state`
- **THEN** the playbook records current, booted, and system-profile targets for selected deployable members without changing any target host

#### Scenario: staged generation is observable after the deploy process exits
- **WHEN** Mandala has staged a generation for reboot and a later survey runs
- **THEN** the snapshot identifies the staged generation through the system-profile target without relying on deploy-run memory
