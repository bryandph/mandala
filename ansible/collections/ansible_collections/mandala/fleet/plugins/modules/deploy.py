#!/usr/bin/python
# -*- coding: utf-8 -*-

DOCUMENTATION = r"""
---
module: deploy
short_description: Run deploy-rs on the controller with optional nom-rendered prebuild
description:
  - Wraps the C(deploy) binary from deploy-rs. Runs in two phases.
  - Phase 1 is an optional prebuild. When C(target) matches C(.#<name>), the action plugin builds C(.#deploy.nodes.<name>.profiles.system.path) with the same dual-mode output as M(mandala.fleet.build). Disable with C(prebuild=false) or override the attribute with C(prebuild_attr).
  - Phase 2 invokes C(deploy) against C(target) (or C(targets) for multi). deploy-rs emits free-form text status, NOT internal-json, so its output is line-streamed raw through ansible's display.
  - When phase 1 runs, C(--skip-checks) is forced for phase 2. The derivation is already in the store; deploy-rs's flake check would duplicate work and often fails in flakes that need C(--impure).
options:
  target:
    description: Single deploy-rs target (e.g. C(.#vishnu)). Mutually exclusive with C(targets).
    type: str
  targets:
    description: Multiple deploy-rs targets (passed as C(--targets a b c)). Mutually exclusive with C(target). Disables auto-prebuild.
    type: list
    elements: str
  dry_activate:
    description: Pass C(--dry-activate) — build and copy but do not activate.
    type: bool
    default: false
  skip_checks:
    description: Pass C(--skip-checks). Implicitly C(true) when prebuild runs.
    type: bool
    default: false
  magic_rollback:
    description: Pass C(--magic-rollback true|false). Unset → deploy-rs default.
    type: bool
  auto_rollback:
    description: Pass C(--auto-rollback true|false). Unset → deploy-rs default.
    type: bool
  confirm_timeout:
    description: Seconds to wait for the C(deploy) confirm step (C(--confirm-timeout)).
    type: int
  hostname:
    description: Override the SSH host for the target (C(--hostname)).
    type: str
  ssh_user:
    description: Override the SSH user for the target (C(--ssh-user)).
    type: str
  remote_build:
    description: Pass C(--remote-build) — build on the target instead of locally.
    type: bool
    default: false
  keep_result:
    description: Pass C(--keep-result) so deploy-rs preserves built outputs.
    type: bool
    default: false
  log_dir:
    description: Pass C(--log-dir) so deploy-rs writes background activation logs to disk (very useful for post-mortem).
    type: path
  cwd:
    description: Working directory for the C(deploy) invocation (and the prebuild, if enabled).
    type: path
  extra_args:
    description: Additional arguments appended to the C(deploy) command line BEFORE C(--) (deploy-rs's own flags).
    type: list
    elements: str
    default: []
  nix_extra_args:
    description: Arguments appended AFTER C(--) — deploy-rs forwards these to its inner C(nix-build) (e.g. C(--impure)).
    type: list
    elements: str
    default: []
  prebuild:
    description: Whether to run the phase-1 build. Defaults to C(true) when C(target) is C(.#<name>), C(false) otherwise.
    type: bool
  prebuild_attr:
    description: Override the auto-derived prebuild attribute. Default is C(<prefix>#deploy.nodes.<name>.profiles.system.path).
    type: str
  output_mode:
    description: Output rendering mode for the prebuild phase. C(auto)|C(tty)|C(flat). See M(mandala.fleet.build).
    type: str
    choices: [auto, tty, flat]
    default: auto
  nom_bin:
    description: Path or name of the C(nix-output-monitor) binary. Used only in C(tty) mode for the prebuild.
    type: str
    default: nom
author:
  - Bryan Prather-Huff (@bryandph)
"""

EXAMPLES = r"""
- name: Deploy vishnu (auto-prebuild on)
  mandala.fleet.deploy:
    target: .#vishnu
    cwd: "{{ playbook_dir }}/.."

- name: Dry-run a deploy of brahma
  mandala.fleet.deploy:
    target: .#brahma
    dry_activate: true

- name: Multi-target deploy (no auto-prebuild)
  mandala.fleet.deploy:
    targets:
      - .#turing-pi-1
      - .#turing-pi-2
    auto_rollback: true

- name: Deploy with --impure forwarded to inner nix-build
  mandala.fleet.deploy:
    target: .#vishnu
    nix_extra_args: [--impure]

- name: Deploy without prebuild (let deploy-rs build directly)
  mandala.fleet.deploy:
    target: .#vishnu
    prebuild: false
    skip_checks: true
"""

RETURN = r"""
rc:
  description: Exit code of the C(deploy) process. On prebuild failure, this is the prebuild's rc.
  returned: always
  type: int
cmd:
  description: Argv of the deploy invocation.
  returned: always
  type: list
  elements: str
build_summary:
  description: Counters from the prebuild phase (see M(mandala.fleet.build) C(summary)). Present only when C(prebuild) ran.
  returned: when prebuild ran
  type: dict
prebuild_out_paths:
  description: Out-paths from the prebuild phase. Present only when C(prebuild) ran successfully.
  returned: when prebuild succeeded
  type: list
  elements: str
build_log:
  description: On prebuild failure, a compact dump of error messages and the largest captured per-derivation build log.
  returned: when prebuild failed
  type: str
"""
