# mandala.fleet

The Ansible surface retained by Mandala is the read-only deployment-state
survey. Native build and deployment orchestration lives in the `mandala`
Rust CLI; this collection does not activate hosts.

## Playbook: `mandala.fleet.state`

`playbooks/state.yml` fans out over the projected inventory's `deploy_rs`
guard group, reads each member's current and booted NixOS generation facts,
and writes one JSON snapshot per host on the controller. Unreachable hosts
are represented in their snapshots instead of failing the entire survey.

The CLI, TUI, and MCP drift views compare these snapshots with locally
evaluated expected systems. The playbook performs only read operations on
managed hosts; creating the controller snapshot directory and writing its
snapshot files are its only filesystem mutations.

```sh
ansible-playbook mandala.fleet.state
ansible-playbook mandala.fleet.state -l k3s
```

Set `MANDALA_FLEET_STATE` to choose the controller snapshot directory.
Otherwise the playbook uses `$XDG_STATE_HOME/mandala/fleet`, falling back to
`~/.local/state/mandala/fleet`.
