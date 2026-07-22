# Pattern: imperative ansible over the projected inventory

The inventory is dynamic and never committed: ansible reads
`.#ansibleInventory` live on every run via a tiny dynamic-inventory
script, so plays always target the fleet as currently declared.

```sh
# inventory/fleet.sh — executable, in ansible.cfg's inventory path
#!/usr/bin/env bash
exec nix eval --no-warn-dirty --json /path/to/fleet#ansibleInventory
```

What the projection gives you:

- **One taxonomy, one spelling.** Groups derive from
  `host.{platform,architecture,category,role,tags}` (plus explicit
  `deployment.ansible.groups`), sanitized to ansible-safe names
  engine-side — `ansible -l k3s`, `.#deployBatch.k3s`, and the CLI's
  `@k3s` are the same member set by construction.
- **The `deploy_rs` guard group.** A synthetic group of the members
  deploy-rs can activate. The read-only `mandala.fleet.state` survey targets
  it so drift snapshots are collected only for members with deploy nodes.
- **NixOS conventions out of the box.** `ansible_python_interpreter`
  pins NixOS members to their system-profile python (no
  `/usr/bin/python3` exists there); override or disable via
  `mandala.ansible.pythonInterpreter`.
- **Repo-specific vars via the hook.** `mandala.ansible.extraHostvars`
  injects per-member vars (e.g. the member's config directory for
  playbooks that write artifacts back into the operator checkout) —
  merged after the engine defaults, so hooks can also override them.

Build and deploy fan-out is native to `mandala deploy`; Ansible remains the
imperative layer for operations such as the read-only `mandala.fleet.state`
survey and operator-authored reboot, onboarding, or maintenance playbooks.
